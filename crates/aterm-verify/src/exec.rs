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
        format!("it {ended} and named no cause (what it printed is in the check's log)")
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
        edits.push(EnvironmentEdit::Set(
            "RUST_TEST_THREADS".into(),
            ceiling("RUST_TEST_THREADS").to_string().into(),
        ));
        edits.push(EnvironmentEdit::Set("BUILD_SLOT_HELD".into(), "1".into()));
    }
    Ok(edits)
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
                c.env(
                    "RUST_TEST_THREADS",
                    ceiling("RUST_TEST_THREADS").to_string(),
                );
                c.env("BUILD_SLOT_HELD", "1");
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
    /// member of its tree.
    fn usage(&self, clock: &Clock<'_>, wall: Duration) -> Option<TreeUsage> {
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
            usage: Some(Usage {
                host,
                pgid,
                cores: host.cores(),
                read_at: now,
                seen: BTreeMap::new(),
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
                && let Some(members) = u.host.group_usage(u.pgid)
            {
                self.book(&members);
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
            .is_some_and(|u| now.saturating_duration_since(u.read_at) >= u.host.usage_every())
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
    /// span is forgotten and nothing moves. A host with no reading ends the reads.
    fn read_usage(&mut self) {
        let Some(u) = &mut self.usage else {
            return;
        };
        let now = self.last;
        let wall = now.saturating_duration_since(u.read_at);
        let worked = std::mem::take(&mut u.worked);
        u.read_at = now;
        let Some(members) = u.host.group_usage(u.pgid) else {
            self.usage = None;
            return;
        };
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
        if self.usage.as_ref().is_some_and(|u| u.read_at < self.last) {
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
pub(crate) mod ends {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The end's flag, and the flag set once a look of this thread's clock finds a wait.
    type Flags = (Arc<AtomicBool>, Arc<AtomicBool>);

    thread_local! {
        static END: std::cell::RefCell<Option<Flags>> = const { std::cell::RefCell::new(None) };
    }

    /// End this thread's run once `flag` is set, setting `seen` once its clock finds a wait.
    pub(crate) fn when(flag: Arc<AtomicBool>, seen: Arc<AtomicBool>) {
        END.with(|e| *e.borrow_mut() = Some((flag, seen)));
    }

    pub(super) fn now() -> bool {
        END.with(|e| {
            e.borrow()
                .as_ref()
                .is_some_and(|(f, _)| f.load(Ordering::SeqCst))
        })
    }

    /// A look of this thread's clock found a wait.
    pub(super) fn saw() {
        END.with(|e| {
            if let Some((_, seen)) = e.borrow().as_ref() {
                seen.store(true, Ordering::SeqCst);
            }
        });
    }
}

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
    elapsed: Duration,
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
    let (case, says) = match &seen {
        Seen::RunningLong { case } => (Some(case.as_str()), None),
        Seen::Red { case, marks } | Seen::Spared { case, marks } => {
            (Some(case.as_str()), block_says(*marks))
        }
        Seen::Undecided { case } => (case.as_deref(), Some("unknown")),
    };
    crate::event_log::emit(
        events,
        &format!(
            "verify-event: {}",
            aterm_json::json!({
                "schema": 1,
                "event": if running_long { "test-case-running-long" } else { "test-case-failed" },
                "outcome": outcome,
                "test": watch.spec, "case": case.map(|name| crate::report::clip_json(name, crate::report::EVENT_ROW_BYTES)),
                "at_unix_ms": aterm_time::unix_millis(),
                "elapsed_s": elapsed.as_secs_f64(),
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
        ),
    );
}

/// Observe a bounded final batch only after the owner has positively reaped its child.
/// No observation here can stop a process or set the returned run's `stopped_at`.
fn observe_reaped(
    cases: &mut test_progress::LiveCases,
    watch: &CaseWatch,
    events: Option<&crate::event_log::EventLog>,
    elapsed: Duration,
) {
    for seen in cases.poll() {
        let evidence = cases.observations().failures.iter().find(|f| match &seen {
            test_progress::Seen::Red { case, .. } | test_progress::Seen::Spared { case, .. } => {
                f.case == *case
            }
            _ => false,
        });
        emit_case(events, watch, &seen, evidence, elapsed, false);
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
    pub(super) mod custody_audit {
        use std::cell::Cell;
        use std::marker::PhantomData;
        use std::rc::Rc;

        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub(crate) struct Report {
            pub(crate) attempts: usize,
            pub(crate) terms: usize,
            pub(crate) kills: usize,
            pub(crate) violations: usize,
        }
        #[derive(Clone, Copy)]
        struct Check {
            pid: u32,
            report: Report,
        }
        thread_local! { static CHECK: Cell<Option<Check>> = const { Cell::new(None) }; }

        pub(crate) struct Scope {
            previous: Option<Check>,
            _same_thread: PhantomData<Rc<()>>,
        }
        impl Scope {
            pub(crate) fn arm(pid: u32) -> Self {
                let previous = CHECK.with(|c| {
                    c.replace(Some(Check {
                        pid,
                        report: Report::default(),
                    }))
                });
                Self {
                    previous,
                    _same_thread: PhantomData,
                }
            }
            pub(crate) fn report(&self) -> Report {
                CHECK.with(|c| c.get().expect("armed custody check").report)
            }
        }
        impl Drop for Scope {
            fn drop(&mut self) {
                CHECK.with(|c| c.set(self.previous));
            }
        }
        pub(super) fn before_signal(pid: u32, sig: i32) -> std::io::Result<()> {
            CHECK.with(|c| {
                let Some(mut check) = c.get() else {
                    return Ok(());
                };
                check.report.attempts += 1;
                let valid = check.pid == pid && super::exited(pid, false).is_ok();
                if valid {
                    check.report.terms += usize::from(sig == super::SIGTERM);
                    check.report.kills += usize::from(sig == super::SIGKILL);
                } else {
                    check.report.violations += 1;
                }
                c.set(Some(check));
                if valid {
                    Ok(())
                } else {
                    Err(std::io::Error::other(
                        "custody fixture refused a signal without its unreaped child",
                    ))
                }
            })
        }
    }

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

    extern "C" fn on_signal(sig: i32) {
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
        watchdog();
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: `on_signal` is an `extern "C" fn(i32)` that lives for the whole program and
            // does only async-signal-safe work.
            unsafe {
                signal(sig, on_signal as extern "C" fn(i32) as usize);
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
    pub(super) mod spared {
        use std::cell::Cell;
        use std::marker::PhantomData;
        use std::rc::Rc;

        thread_local! { static ARMED: Cell<bool> = const { Cell::new(false) }; }

        pub(crate) struct Scope {
            previous: bool,
            _same_thread: PhantomData<Rc<()>>,
        }
        impl Scope {
            pub(crate) fn arm() -> Self {
                Self {
                    previous: ARMED.with(|armed| armed.replace(true)),
                    _same_thread: PhantomData,
                }
            }
        }
        impl Drop for Scope {
            fn drop(&mut self) {
                ARMED.with(|armed| armed.set(self.previous));
            }
        }
        pub(super) fn armed() -> bool {
            ARMED.with(Cell::get)
        }
    }
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
mod tests {
    use super::*;

    /// A VARIABLE SET TWICE IS SET ONCE, to its last value, where it was first set: what the child
    /// sees, named once. NEGATIVE CONTROL: two variables keep both, in the order they were set.
    #[test]
    fn a_variable_set_twice_is_named_once_with_its_last_value() {
        let cmd = Cmd::new("targo")
            .env("ATERM_SPEC_CHECKERS", "1")
            .env("TRAIL_LAWS_FULL", "1")
            .env("ATERM_SPEC_CHECKERS", "0");
        assert_eq!(
            cmd.envs,
            [
                ("ATERM_SPEC_CHECKERS".into(), "0".into()),
                ("TRAIL_LAWS_FULL".into(), "1".into())
            ]
        );
    }

    /// THE MACHINE'S CORES time a child's tree: the live host's count is `aterm_capacity::cores`,
    /// sysprobe's logical cores, not this process's parallelism (`Ctx::parallelism`), which an
    /// affinity mask or a cgroup quota narrows. The layout reads on macOS and Linux, so the count
    /// is the machine's and not the floor of one that stands in for an unread layout. NEGATIVE
    /// CONTROL, on Linux: a thread narrowed to one core of a machine of more reads a parallelism of
    /// one and the live host's count unchanged, so a count read from the parallelism fails. macOS
    /// has no affinity that narrows it, so there the two counts agree and only the first half
    /// discriminates.
    #[test]
    fn the_live_hosts_cores_are_the_machines_logical_cores() {
        let logical = aterm_sysprobe::cpu_layout().logical_cores();
        let machine = u32::try_from(logical).unwrap_or(u32::MAX).max(1);
        assert_eq!(LiveHost.cores(), machine);
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(logical > 0, "the layout reads on this host");
        }
        #[cfg(target_os = "linux")]
        if logical > 1 {
            let (parallelism, cores) = std::thread::spawn(|| {
                unsafe extern "C" {
                    /// `<sched.h>`; aterm-libc does not export it. `mask` is a `cpu_set_t`.
                    fn sched_getaffinity(pid: i32, size: usize, mask: *mut [u64; 16]) -> i32;
                    /// `<sched.h>`.
                    fn sched_setaffinity(pid: i32, size: usize, mask: *const [u64; 16]) -> i32;
                }
                let mut allowed = [0u64; 16];
                // SAFETY: `allowed` is a writable 1024-bit `cpu_set_t` of the size passed; pid 0
                // is this thread.
                let rc = unsafe {
                    sched_getaffinity(0, std::mem::size_of_val(&allowed), &raw mut allowed)
                };
                assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
                let word = allowed
                    .iter()
                    .position(|w| *w != 0)
                    .expect("an allowed core");
                let mut one = [0u64; 16];
                one[word] = 1 << allowed[word].trailing_zeros();
                // SAFETY: `one` is a readable `cpu_set_t` of the size passed, naming one core
                // this thread may run on; pid 0 narrows this thread alone.
                let rc =
                    unsafe { sched_setaffinity(0, std::mem::size_of_val(&one), &raw const one) };
                assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
                (
                    std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get),
                    LiveHost.cores(),
                )
            })
            .join()
            .expect("the narrowed thread");
            assert_eq!(parallelism, 1, "the thread is narrowed to one core");
            assert_eq!(
                cores, machine,
                "the live host still reads the machine's cores"
            );
        }
    }

    /// A command-local removal wins over the run's inherited additions, while an explicit value
    /// on that same command wins over its removal, matching `Command`. NEGATIVE CONTROL: an
    /// unrelated variable remains present.
    #[test]
    fn a_command_can_remove_one_inherited_variable_without_clearing_the_rest() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let removed = run(
            &Cmd::new("/usr/bin/env").remove_env("PATH"),
            env_in(dir.path()),
        );
        assert!(removed.ok, "{}", removed.output);
        assert!(!removed.output.lines().any(|line| line.starts_with("PATH=")));
        assert!(
            removed.output.lines().any(|line| line.starts_with("HOME=")),
            "an unrelated inherited variable remains"
        );
        let restored = run(
            &Cmd::new("/usr/bin/env")
                .remove_env("PATH")
                .env("PATH", "/restored"),
            env_in(dir.path()),
        );
        assert!(restored.output.lines().any(|line| line == "PATH=/restored"));
    }

    #[cfg(unix)]
    #[test]
    fn a_native_stage_keeps_its_inherited_pipe_descriptor() {
        use std::io::Write as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
        let dir = aterm_tempfile::tempdir().unwrap();
        let (reader, mut writer) = std::io::pipe().unwrap();
        // SAFETY: F_DUPFD creates one owned, non-close-on-exec duplicate without replacing
        // another descriptor. The original remains live throughout the call.
        let fd = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_DUPFD, 100) };
        assert!(fd >= 100);
        // SAFETY: the successful duplicate is newly owned by this test alone.
        let inherited = unsafe { OwnedFd::from_raw_fd(fd) };
        writer.write_all(b"shared-jobserver-handle\n").unwrap();
        drop(writer);
        let result = run(
            &Cmd::new("/bin/cat").arg(format!("/dev/fd/{fd}")),
            env_in(dir.path()),
        );
        assert!(result.ok, "{}", result.output);
        assert_eq!(result.output, "shared-jobserver-handle\n");
        drop(inherited);
    }

    fn env_in(dir: &Path) -> ExecEnv<'_> {
        ceiled(dir, Some(DEFAULT_CHILD_CEILING))
    }

    #[cfg(unix)]
    #[test]
    fn a_moldable_compile_passes_its_admitted_width_to_cargo_and_restores_it() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let cmd = Cmd::new("/usr/bin/printenv")
            .arg("CARGO_BUILD_JOBS")
            .env("CARGO_BUILD_JOBS", "16");
        let narrowed = crate::compile_width::with_jobs(Some(2), || run(&cmd, env_in(dir.path())));
        assert!(narrowed.ok, "{}", narrowed.output);
        assert_eq!(narrowed.output.trim(), "2");
        let restored = run(&cmd, env_in(dir.path()));
        assert!(restored.ok, "{}", restored.output);
        assert_eq!(restored.output.trim(), "16");
    }

    #[cfg(unix)]
    #[test]
    fn compiler_admission_refusal_is_environmental_but_a_compiler_error_is_not() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut failed = run(&Cmd::new("/usr/bin/false"), env_in(dir.path()));
        failed.output = "aterm-verify: compiler admission: unit memory history disappeared\nerror: could not compile\n".into();
        assert_eq!(
            crate::compile_width::refusal(&failed, "").as_deref(),
            Some("unit memory history disappeared")
        );
        // The marker is meaningful only to the compile stage, never to a failed test.
        assert_eq!(failed.environment_failure(), None);
        assert_eq!(
            crate::compile_width::refusal(
                &failed,
                r#"{"reason":"compiler-message","message":{"level":"error"}}"#
            ),
            None
        );
        failed.output = "error: unresolved import\n".into();
        assert_eq!(crate::compile_width::refusal(&failed, ""), None);
        failed.output = "source: aterm-verify: compiler admission: quoted source text\n".into();
        assert_eq!(crate::compile_width::refusal(&failed, ""), None);
    }

    fn ceiled(dir: &Path, child_ceiling: Option<Duration>) -> ExecEnv<'_> {
        ExecEnv {
            events: None,
            cwd: dir,
            path: OsStr::new("/usr/bin:/bin"),
            scratch: dir,
            child_ceiling,
            remove_env: &[],
            add_env: &[],
            stage_groups: None,
            host: &INERT,
            hang_samples: None,
        }
    }

    /// A host that reads nothing, or only the input clock it is given: every span with no marked
    /// wait is work, as before the clock read its tree.
    #[derive(Debug)]
    pub(super) struct Idle(pub(super) Option<Duration>);

    impl Host for Idle {
        fn group_usage(&self, _: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
            None
        }

        fn idle(&self) -> Option<Duration> {
            self.0
        }

        fn cores(&self) -> u32 {
            1
        }
    }

    /// No reading of anything.
    pub(super) static INERT: Idle = Idle(None);

    /// A stand-in host for a tree that is runnable all the time and is given `share` per mille of
    /// that time on a core: the child's group as one member, whose counters run with the wall
    /// clock between reads, or, booked in `lumps`, stay flat until that long has passed since they
    /// last grew and then grow by all of it, as macOS books a starved thread's time when it next
    /// gets a core.
    #[cfg(unix)]
    #[derive(Debug)]
    struct Starving {
        share: AtomicU64,
        lumps: Option<Duration>,
        counters: std::sync::Mutex<Option<(Instant, u64, u64)>>,
    }

    #[cfg(unix)]
    impl Starving {
        fn at(per_mille: u64) -> &'static Self {
            Self::booked(per_mille, None)
        }

        fn lumpy(per_mille: u64, every: Duration) -> &'static Self {
            Self::booked(per_mille, Some(every))
        }

        fn booked(per_mille: u64, lumps: Option<Duration>) -> &'static Self {
            Box::leak(Box::new(Self {
                share: AtomicU64::new(per_mille),
                lumps,
                counters: std::sync::Mutex::new(None),
            }))
        }

        fn give(&self, per_mille: u64) {
            self.share.store(per_mille, Ordering::SeqCst);
        }
    }

    #[cfg(unix)]
    impl Host for Starving {
        fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
            let mut c = self
                .counters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let (at, cpu, runnable) = c.get_or_insert((now, 0, 0));
            let since = now.saturating_duration_since(*at);
            if self.lumps.is_none_or(|every| since >= every) {
                let span = u64::try_from(since.as_nanos()).unwrap_or(u64::MAX);
                *at = now;
                *runnable += span;
                *cpu += span / 1000 * self.share.load(Ordering::SeqCst);
            }
            Some(vec![aterm_sysprobe::MemberUsage {
                pid: pgid,
                start: 1,
                cpu_ns: *cpu,
                runnable_ns: *runnable,
                io_bytes: 0,
                ..aterm_sysprobe::MemberUsage::default()
            }])
        }

        fn idle(&self) -> Option<Duration> {
            None
        }

        fn cores(&self) -> u32 {
            1
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_starred_removal_takes_every_inherited_variable_it_begins() {
        let tmp = crate::mktemp_dir("atv-env-prefix").expect("mktemp");
        let env_of = |remove: &[&str]| {
            let env = ExecEnv {
                remove_env: remove,
                ..ceiled(&tmp, Some(Duration::from_secs(60)))
            };
            run(&Cmd::new("/usr/bin/env"), env).output
        };
        let removed = env_of(&["CARGO_PKG_*", "CARGO_MANIFEST_DIR"]);
        assert!(
            !removed
                .lines()
                .any(|l| l.starts_with("CARGO_PKG_") || l.starts_with("CARGO_MANIFEST_DIR=")),
            "{removed}"
        );
        if std::env::var_os("CARGO_PKG_NAME").is_some() {
            let kept = env_of(&["CARGO_PKG"]);
            assert!(
                kept.lines().any(|l| l.starts_with("CARGO_PKG_NAME=")),
                "{kept}"
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The host shell's argv for a script: `unix` under `/bin/sh -c`, or off Unix `windows` under
    /// `cmd.exe /d /s /c`, which strips the quotes std puts round an argument with a space and
    /// leaves a variable it does not have as its literal `%NAME%`.
    fn shell_argv(unix: &str, windows: &str) -> Vec<OsString> {
        if cfg!(unix) {
            vec!["/bin/sh".into(), "-c".into(), unix.into()]
        } else {
            let cmd = std::env::var_os("ComSpec")
                .unwrap_or_else(|| OsString::from(r"C:\Windows\System32\cmd.exe"));
            vec![cmd, "/d".into(), "/s".into(), "/c".into(), windows.into()]
        }
    }

    fn shell(unix: &str, windows: &str) -> Cmd {
        let argv = shell_argv(unix, windows);
        Cmd::new(&argv[0]).args(&argv[1..])
    }

    /// What a child said, its line ends LF as `/bin/sh`'s are (cmd.exe ends each with CRLF).
    fn said(r: &Run) -> String {
        let text = if cfg!(windows) {
            r.output.replace("\r\n", "\n")
        } else {
            r.output.clone()
        };
        text.trim_end_matches('\n').to_owned()
    }

    /// What the host shell's script reads for a variable the child does not have: the script's
    /// `${NAME-unset}`, or cmd.exe's untouched `%NAME%`.
    fn unset(name: &str) -> String {
        if cfg!(unix) {
            "unset".to_owned()
        } else {
            format!("%{name}%")
        }
    }

    /// A variable every test run inherits, which a child that keeps only its record must not.
    const INHERITED: &str = if cfg!(unix) { "HOME" } else { "USERPROFILE" };

    /// Only the isolated fixture reads its inherited dispositions; ordinary suite runs do not
    /// assert anything about the launcher that started libtest.
    #[cfg(unix)]
    #[test]
    fn signal_disposition_probe() {
        let Ok(expected) = std::env::var("ATERM_VERIFY_SIGNAL_PROBE") else {
            return;
        };
        let expected = if expected == "ignored" {
            libc::SIG_IGN
        } else {
            libc::SIG_DFL
        };
        for sig in [
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGUSR1,
            libc::SIGUSR2,
            libc::SIGALRM,
        ] {
            let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
            // SAFETY: a null new action only reads the disposition into the live local.
            assert_eq!(
                unsafe { libc::sigaction(sig, std::ptr::null(), action.as_mut_ptr()) },
                0
            );
            // SAFETY: the successful query initialized the action.
            assert_eq!(
                unsafe { action.assume_init() }.sa_sigaction,
                expected,
                "signal {sig}"
            );
        }
        println!("\nSIGNALS_CHECKED");
    }

    /// Verify's actual test runner and direct measurement commands discard a parent's ignores.
    /// The ordinary spawn is the negative control: it must observe SIG_IGN, not SIG_DFL.
    #[cfg(unix)]
    #[test]
    fn test_runner_defaults_signals_from_an_ignoring_parent() {
        const PARENT: &str = "ATERM_VERIFY_SIGNAL_PARENT";
        const NAME: &str = "exec::tests::test_runner_defaults_signals_from_an_ignoring_parent";
        const PROBE: &str = "exec::tests::signal_disposition_probe";
        let me = std::env::current_exe().unwrap();
        if std::env::var_os(PARENT).is_none() {
            let mut parent = aterm_uds::spawnfd::command("/bin/sh");
            parent
                .args([
                    "-c",
                    "trap '' INT QUIT TERM HUP USR1 USR2 ALRM PIPE; exec \"$0\" \"$@\"",
                ])
                .arg(&me)
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(PARENT, "1");
            let out = aterm_uds::deadline::ends_within(&mut parent, Duration::from_secs(60));
            assert!(out.status.success(), "{out:?}");
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("\nSIGNAL_RUNNER_CHECKED\n"),
                "{out:?}"
            );
            return;
        }
        let args = ["--exact", PROBE, "--nocapture", "--test-threads=1"];
        let mut control = aterm_uds::spawnfd::command(&me);
        control
            .args(args)
            .env("ATERM_VERIFY_SIGNAL_PROBE", "ignored");
        let out = aterm_uds::deadline::ends_within(&mut control, Duration::from_secs(60));
        assert!(out.status.success(), "inherited-ignore control: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("\nSIGNALS_CHECKED\n"),
            "{out:?}"
        );

        let dir = aterm_tempfile::tempdir().unwrap();
        let cmd = Cmd::new(&me)
            .args(args)
            .env("ATERM_VERIFY_SIGNAL_PROBE", "default");
        let ran = run_test(&cmd, env_in(dir.path()), &|| {});
        assert!(ran.ok, "test runner: {}", ran.output);
        assert!(ran.output.contains("\nSIGNALS_CHECKED\n"), "{}", ran.output);

        let mut measured = check_command(&me);
        measured
            .args(args)
            .env("ATERM_VERIFY_SIGNAL_PROBE", "default");
        let out = aterm_uds::deadline::ends_within(&mut measured, Duration::from_secs(60));
        assert!(out.status.success(), "measurement runner: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("\nSIGNALS_CHECKED\n"),
            "{out:?}"
        );
        println!("\nSIGNAL_RUNNER_CHECKED");
    }

    #[test]
    fn a_child_runs_in_the_root_with_the_runs_variables_and_its_own_win() {
        let tmp = crate::mktemp_dir("atv-setenv").expect("mktemp");
        std::fs::write(tmp.join("marker"), b"x").expect("write");
        let added = [(OsString::from("ATV_ADDED_PROBE"), OsString::from("0"))];
        let env = ExecEnv {
            add_env: &added,
            ..env_in(&tmp)
        };
        let ask = shell(
            "cat marker; printf ' %s %s' \"${ATV_ADDED_PROBE-unset}\" \"$PATH\"",
            "type marker& echo  %ATV_ADDED_PROBE% %PATH%",
        );
        assert_eq!(said(&run(&ask, env)), "x 0 /usr/bin:/bin");
        let stage_says = ask.clone().env("ATV_ADDED_PROBE", "1");
        assert_eq!(said(&run(&stage_says, env)), "x 1 /usr/bin:/bin");
        assert_eq!(
            said(&run(&ask, env_in(&tmp))),
            format!("x {} /usr/bin:/bin", unset("ATV_ADDED_PROBE"))
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A child is handed the cover marker of its stage's claim ([`crate::memory::cover`]) over
    /// the run's, from the stage's own thread and from a worker of its pool alike. NEGATIVE
    /// CONTROL: outside a stage that holds a claim, the run's own (`NO_COVER`) reaches it.
    #[test]
    fn a_child_is_handed_its_stages_cover_marker() {
        let tmp = crate::mktemp_dir("atv-cover").expect("mktemp");
        let added = [(
            OsString::from(aterm_capacity::COVER_ENV),
            OsString::from(aterm_capacity::NO_COVER),
        )];
        let env = ExecEnv {
            add_env: &added,
            ..env_in(&tmp)
        };
        let ask = shell(
            "printf %s \"$ATERM_CAPACITY_COVER\"",
            "echo %ATERM_CAPACITY_COVER%",
        );
        assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
        {
            let _covering = crate::memory::covering(Some("0000000000001-7-0".into()));
            assert_eq!(said(&run(&ask, env)), "0000000000001-7-0");
            let pooled = crate::testrun::pool(1, &[0], 1, |_| said(&run(&ask, env)));
            assert_eq!(pooled, [Some("0000000000001-7-0".to_string())]);
            let dispatched = crate::testrun::dispatch_each(1, |_| said(&run(&ask, env)));
            assert_eq!(dispatched, [Some("0000000000001-7-0".to_string())]);
        }
        assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A child of a combined stage claim receives that claim's actual CPU grant, including
    /// through the bounded test workers that retain it. A command's smaller explicit ceiling is
    /// preserved. NEGATIVE CONTROL: propagating the requested machine width, omitting the held
    /// marker, or losing custody on a worker fails an exact environment assertion.
    #[test]
    fn a_child_is_bounded_by_its_stages_actual_core_grant() {
        let tmp = crate::mktemp_dir("atv-resource-grant").expect("mktemp");
        let ledger = aterm_capacity::Ledger::at(tmp.join("ledger"));
        let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
        let ordinary = aterm_capacity::admit_with(
            &ledger,
            &machine,
            aterm_capacity::Need::new("ordinary cores").cores(8),
            aterm_capacity::Admit::Wait,
            aterm_capacity::Pace::default(),
            &mut |_| {},
        )
        .expect("ordinary claim");
        let stage = aterm_capacity::admit_with(
            &ledger,
            &machine,
            aterm_capacity::Need::new("background stage")
                .mem_gib(4)
                .cores(14)
                .covers(),
            aterm_capacity::Admit::Wait,
            aterm_capacity::Pace::default(),
            &mut |_| {},
        )
        .expect("stage claim");
        assert_eq!(
            stage.cores(),
            6,
            "the fixture must exercise a partial grant"
        );
        let _resources = crate::holding_resource_claim(Some(std::sync::Arc::new(stage)));
        let ask = shell(
            "printf '%s %s %s' \"$CARGO_BUILD_JOBS\" \"$BUILD_SLOT_HELD\" \"$RUST_TEST_THREADS\"",
            "echo %CARGO_BUILD_JOBS% %BUILD_SLOT_HELD% %RUST_TEST_THREADS%",
        );
        assert_eq!(said(&run(&ask, env_in(&tmp))), "6 1 6");
        assert_eq!(
            said(&run(
                &ask.clone().env("CARGO_BUILD_JOBS", "3"),
                env_in(&tmp)
            )),
            "3 1 6"
        );
        assert_eq!(
            said(&run(
                &ask.clone().env("RUST_TEST_THREADS", "2"),
                env_in(&tmp)
            )),
            "6 1 2"
        );
        let added = [("RUST_TEST_THREADS".into(), "4".into())];
        let mut with_run_ceiling = env_in(&tmp);
        with_run_ceiling.add_env = &added;
        assert_eq!(said(&run(&ask, with_run_ceiling)), "6 1 4");
        assert_eq!(
            crate::testrun::pool(1, &[0], 1, |_| said(&run(&ask, env_in(&tmp)))),
            [Some("6 1 6".to_owned())]
        );
        assert_eq!(
            crate::testrun::dispatch_each(1, |_| said(&run(&ask, env_in(&tmp)))),
            [Some("6 1 6".to_owned())]
        );
        #[cfg(unix)]
        {
            let argv = shell_argv(
                "printf '%s %s %s %s %s' \"$CARGO_BUILD_JOBS\" \"$BUILD_SLOT_HELD\" \
                 \"$RUST_TEST_THREADS\" \"$ATERM_CAPACITY_COVER\" \"${ATERM_WAITS-unset}\"",
                "",
            );
            let recorded = Cmd::replayed(
                &argv,
                &tmp,
                vec![
                    ("CARGO_BUILD_JOBS".into(), "13".into()),
                    ("CARGO_BUILD_JOBS".into(), "12".into()),
                    ("RUST_TEST_THREADS".into(), "11".into()),
                    ("BUILD_SLOT_HELD".into(), "stale".into()),
                    (aterm_capacity::COVER_ENV.into(), "stale".into()),
                    (aterm_toolchain::waits::VAR.into(), "/stale/waits".into()),
                ],
            );
            assert!(
                finalize_exact_command(recorded.clone(), env_in(&tmp)).is_err(),
                "a dynamic per-spawn wait directory cannot be content-keyed"
            );
            let _covering = crate::memory::covering(Some("0000000000001-7-0".into()));
            let finalized = finalize_exact_command(recorded.counting_waits(), env_in(&tmp))
                .expect("the counting command has a representable exact environment");
            let value = |name: &str| {
                finalized
                    .envs
                    .iter()
                    .filter(|(held, _)| held == name)
                    .map(|(_, value)| value.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            };
            assert_eq!(value("CARGO_BUILD_JOBS"), ["6"]);
            assert_eq!(value("RUST_TEST_THREADS"), ["6"]);
            assert_eq!(value("BUILD_SLOT_HELD"), ["1"]);
            assert_eq!(value(aterm_capacity::COVER_ENV), ["0000000000001-7-0"]);
            assert!(value(aterm_toolchain::waits::VAR).is_empty());
            assert_eq!(
                said(&run(&finalized, env_in(&tmp))),
                "6 1 6 0000000000001-7-0 unset",
                "the materialized command and the child receive the same post-claim environment"
            );
        }
        drop(_resources);

        #[cfg(unix)]
        {
            let argv = shell_argv("printf %s \"$CARGO_BUILD_JOBS\"", "");
            let recorded =
                Cmd::replayed(&argv, &tmp, vec![("CARGO_BUILD_JOBS".into(), "12".into())])
                    .counting_waits();
            let finalized = crate::compile_width::with_jobs(Some(4), || {
                finalize_exact_command(recorded, env_in(&tmp))
            })
            .expect("the compiler-width configurator has a representable environment");
            assert!(finalized.runtime_environment_is_finalized());
            assert!(
                !finalized
                    .clone()
                    .env("CARGO_BUILD_JOBS", "99")
                    .runtime_environment_is_finalized(),
                "an environment builder mutation must thaw the receipt"
            );
            for changed in [
                finalized.clone(),
                finalized.clone(),
                finalized.clone(),
                finalized.clone(),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, mut command)| {
                match index {
                    0 => command.args.push("changed-argv".into()),
                    1 => command.cwd = Some(tmp.join("changed-cwd")),
                    2 => command.demoted = true,
                    3 => command.envs.push(("RAW_MUTATION".into(), "1".into())),
                    _ => unreachable!(),
                }
                command
            }) {
                assert!(
                    !changed.runtime_environment_is_finalized(),
                    "a public command-field mutation must invalidate the receipt"
                );
            }
            assert_eq!(
                crate::compile_width::with_jobs(Some(2), || {
                    said(&run(&finalized, env_in(&tmp)))
                }),
                "4",
                "dispatch must execute the finalized environment, not recompute later policy"
            );
        }
        drop(ordinary);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A child replayed from a recorded invocation ([`Cmd::replayed`]) is handed the cover marker
    /// of its stage's claim over the one its recorded environment holds, from a worker of the
    /// test dispatch too. NEGATIVE CONTROL: outside a stage that holds a claim, its recorded one
    /// reaches it.
    #[cfg(unix)]
    #[test]
    fn a_replayed_child_is_handed_its_stages_cover_marker() {
        let tmp = crate::mktemp_dir("atv-replayed-cover").expect("mktemp");
        let env = env_in(&tmp);
        let ask = Cmd::replayed(
            &[
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("printf %s \"$ATERM_CAPACITY_COVER\""),
            ],
            &tmp,
            vec![(
                OsString::from(aterm_capacity::COVER_ENV),
                OsString::from(aterm_capacity::NO_COVER),
            )],
        );
        assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
        {
            let _covering = crate::memory::covering(Some("0000000000001-7-0".into()));
            assert_eq!(said(&run(&ask, env)), "0000000000001-7-0");
            let dispatched = crate::testrun::dispatch_each(1, |_| said(&run(&ask, env)));
            assert_eq!(dispatched, [Some("0000000000001-7-0".to_string())]);
        }
        assert_eq!(said(&run(&ask, env)), aterm_capacity::NO_COVER);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_child_killed_by_a_signal_says_which_after_what_it_wrote() {
        let tmp = crate::mktemp_dir("atv-signalled").expect("mktemp");
        let run = run(
            &Cmd::new("/bin/sh").args(["-c", "echo started; kill -KILL $$"]),
            env_in(&tmp),
        );
        assert!(!run.ok && run.code.is_none(), "{run:?}");
        assert_eq!(
            run.trimmed_output(),
            "started\naterm-verify: the child was killed by signal 9"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_missing_tool_is_a_failed_run_not_a_panic_and_never_a_pass() {
        let tmp = crate::mktemp_dir("atv-exec3").expect("mktemp");
        let translator = aterm_uds::process::translator();
        let mut mute = Vec::new();
        for (row, cmd) in [
            ("plain", Cmd::new(tmp.join("absent-driver"))),
            ("demoted", Cmd::new(tmp.join("absent-driver")).demoted()),
        ] {
            let r = run(&cmd, env_in(&tmp));
            assert!(!r.ok, "{row}: {r:?}");
            if translator.is_some() && r.spawn_error.is_none() && r.code == Some(127) {
                mute.push(row);
                continue;
            }
            assert!(r.spawn_error.is_some(), "{row}: {r:?}");
        }
        if let Some(translator) = translator
            && !mute.is_empty()
        {
            println!(
                "NOT RUN under {translator}: exec::tests::\
                 a_missing_tool_is_a_failed_run_not_a_panic_and_never_a_pass: the spawn error \
                 ({}): std's answer comes from glibc's posix_spawn, whose child reports a failed \
                 exec through memory it shares with its parent; the translator runs that child in \
                 a copy, where the report is lost, so std answers a child that exited 127 (still a \
                 failed run); a real CPU of this code's arch decides it",
                mute.join(", ")
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn output_is_captured_with_stderr_merged_and_a_nonzero_exit_keeps_its_code() {
        let tmp = crate::mktemp_dir("atv-exec").expect("mktemp");
        let r = run(
            &shell(
                "echo out; echo err >&2; echo out2",
                "echo out& (echo err)1>&2& echo out2",
            ),
            env_in(&tmp),
        );
        assert!(r.ok, "{r:?}");
        assert_eq!(said(&r), "out\nerr\nout2");
        let r = run(
            &shell("echo why >&2; exit 3", "(echo why)1>&2& exit 3"),
            env_in(&tmp),
        );
        assert!(!r.ok);
        assert_eq!(said(&r), "why");
        assert_eq!(r.code, Some(3));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_replayed_child_starts_where_and_as_it_was_recorded() {
        let tmp = crate::mktemp_dir("atv-replay").expect("mktemp");
        let pkg = tmp.join("pkg");
        std::fs::create_dir_all(&pkg).expect("mkdir");
        std::fs::write(pkg.join("marker"), b"in-pkg").expect("write");
        let added = [(OsString::from("GATE_ADDED"), OsString::from("gate"))];
        let env = ExecEnv {
            add_env: &added,
            ..env_in(&tmp)
        };
        assert!(
            std::env::var_os(INHERITED).is_some(),
            "the probe needs {INHERITED} where the replay could inherit it"
        );
        let argv = shell_argv(
            &format!(
                "cat marker; printf ' %s %s %s' \"${{RECORDED-unset}}\" \"${{GATE_ADDED-unset}}\" \
                 \"${{{INHERITED}-unset}}\""
            ),
            &format!("type marker& echo  %RECORDED% %GATE_ADDED% %{INHERITED}%"),
        );
        let cmd = Cmd::replayed(
            &argv,
            &pkg,
            vec![
                (OsString::from("RECORDED"), OsString::from("yes")),
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            ],
        )
        .preamble("     Running tests/x.rs (target/debug/deps/x-1)\n");
        let r = run(&cmd, env);
        assert!(r.ok, "{}", r.output);
        assert_eq!(
            said(&r),
            format!(
                "     Running tests/x.rs (target/debug/deps/x-1)\nin-pkg yes {} {}",
                unset("GATE_ADDED"),
                unset(INHERITED)
            ),
            "the preamble first, then the child — in its own directory, with only what \
             was recorded"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_flag_moves_the_ceiling_and_only_a_typed_word_removes_it() {
        for junk in ["", "  ", "45m", "soon", "-5", "NaN", "1e400", "4e9"] {
            assert_eq!(
                parse_ceiling(junk),
                None,
                "{junk:?} is not a number of seconds, and must not disable the ceiling"
            );
        }
        for off in ["0", "0.0", "off", "OFF", "none", "Never", " off "] {
            assert_eq!(
                parse_ceiling(off),
                Some(None),
                "{off:?} must disable the ceiling"
            );
        }
        assert_eq!(parse_ceiling("90"), Some(Some(Duration::from_secs(90))));
        assert_eq!(
            parse_ceiling("0.25"),
            Some(Some(Duration::from_millis(250)))
        );
    }

    #[cfg(unix)]
    const GONE: Duration = Duration::from_secs(60);

    #[cfg(unix)]
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    #[cfg(unix)]
    fn gone_within(pid: &str, window: Duration) -> bool {
        let deadline = Instant::now() + window;
        loop {
            let stat = std::process::Command::new(aterm_uds::os_path!("ps"))
                .args(["-o", "stat=", "-p", pid])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if stat.is_empty() || stat.starts_with('Z') {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(unix)]
    fn written_pid(file: &Path) -> Option<String> {
        let t = std::fs::read_to_string(file).ok()?;
        t.ends_with('\n').then(|| t.trim().to_string())
    }

    #[cfg(unix)]
    fn read_pid(file: &Path) -> String {
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            if let Some(pid) = written_pid(file) {
                return pid;
            }
            assert!(Instant::now() < deadline, "no pid in {}", file.display());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    fn with_grandchild(file: &Path) -> Cmd {
        Cmd::new("/bin/sh").args([
            "-c",
            &format!("sleep 600 & echo $! > '{}'; wait", file.display()),
        ])
    }

    #[cfg(unix)]
    #[test]
    fn a_ceiling_kill_takes_the_childs_whole_process_group() {
        let tmp = crate::mktemp_dir("atv-ceil-group").expect("mktemp");
        let mut ceiling = Duration::from_millis(500);
        let grandchild = loop {
            let pidfile = tmp.join(format!("grandchild-{}ms.pid", ceiling.as_millis()));
            let r = run(&with_grandchild(&pidfile), ceiled(&tmp, Some(ceiling)));
            assert!(
                !r.ok && r.output.contains(crate::findings::CEILING_KILL),
                "{}",
                r.output
            );
            if let Some(pid) = written_pid(&pidfile) {
                break pid;
            }
            assert!(
                ceiling < GONE,
                "no shell backgrounded its grandchild within a {ceiling:?} ceiling"
            );
            ceiling *= 2;
        };
        assert!(
            gone_within(&grandchild, GONE),
            "the grandchild {grandchild} outlived the ceiling kill"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A child of `/bin/sleep 600` leading its own group, under a run in `tmp`.
    #[cfg(unix)]
    fn a_sleeping_leader(tmp: &Path) -> Child {
        spawn_command(
            &Cmd::new("/bin/sleep").arg("600"),
            env_in(tmp),
            Path::new(TASKPOLICY),
        )
        .spawn()
        .expect("spawn sleep")
    }

    /// `pid`'s group, `SIGKILL`ed by `/bin/kill` once a test's wait on it ran past its hang
    /// detector, so the thread blocked on its reap returns.
    #[cfg(unix)]
    fn kill_stray_group(pid: u32) {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status();
    }

    /// The reap after `SIGKILL` is bounded: a child still there at the bound is left as it is,
    /// named, and the wait returns; once it is killed, the same reap takes it.
    #[cfg(unix)]
    #[test]
    fn a_reap_after_sigkill_gives_up_at_its_bound_and_leaves_the_child_as_it_is() {
        let tmp = crate::mktemp_dir("atv-bounded-reap").expect("mktemp");
        let mut child = a_sleeping_leader(&tmp);
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        let reaper = std::thread::spawn(move || {
            let mut live = group::Live::enter(child.id());
            let left = reap_killed(&mut child, &mut live, Duration::from_millis(200));
            let _ = tx.send(left);
            let _ = live.kill();
            reap_killed(&mut child, &mut live, HANG_DETECTOR)
        });
        let Ok(left) = rx.recv_timeout(HANG_DETECTOR) else {
            kill_stray_group(pid);
            let _ = reaper.join();
            panic!("the reap waited past its bound");
        };
        let left = left.expect("a child still there at the bound");
        assert!(left.contains(&format!("pid {pid},")), "{left}");
        assert_eq!(
            reaper.join().expect("the reaper"),
            None,
            "the killed child reaped"
        );
        assert!(gone_within(&pid.to_string(), HANG_DETECTOR));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A ceiling's kill whose `SIGKILL` does not end the leader (a stand-in: the `SIGKILL` a
    /// [`group::spared::Scope`] sends nowhere, as macOS's held first exec may not die of it) gives
    /// up its reap at its bound and returns, its block naming the leader it left as it is; that
    /// leader is still verify's to kill and reap. Negative control: the unbounded reap the kill
    /// had (`reap_stage(child, true)`) never returns, which the hang detector fails.
    #[cfg(unix)]
    #[test]
    fn a_ceiling_kill_whose_sigkill_does_not_end_the_leader_names_it_and_returns() {
        let tmp = crate::mktemp_dir("atv-spared-kill").expect("mktemp");
        let mut child = a_sleeping_leader(&tmp);
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        let killer = std::thread::spawn(move || {
            let mut live = group::Live::enter(child.id());
            let note = {
                let _spared = group::spared::Scope::arm();
                kill_and_reap(&mut child, &mut live, None, Duration::from_secs(1))
            };
            let _ = tx.send(note);
            let _ = live.kill();
            reap_killed(&mut child, &mut live, HANG_DETECTOR)
        });
        let Ok(note) = rx.recv_timeout(HANG_DETECTOR) else {
            kill_stray_group(pid);
            let _ = killer.join();
            panic!("the kill's reap waited past its bound");
        };
        assert_eq!(
            note,
            format!("  (it did not end within 1s of SIGKILL: pid {pid}, left as it is)\n")
        );
        assert_eq!(
            killer.join().expect("the killer"),
            None,
            "the leader, killed, reaped"
        );
        assert!(gone_within(&pid.to_string(), HANG_DETECTOR));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[cfg(unix)]
    fn suite_with_trap(marker: &Path, pidfile: &Path, ignore: bool) -> Cmd {
        Cmd::new("/bin/bash").args([
            "-c",
            &format!(
                "{}trap 'touch {}' EXIT; sleep 600 & echo $! > '{}'; wait",
                if ignore { "trap '' TERM; " } else { "" },
                marker.display(),
                pidfile.display()
            ),
        ])
    }

    #[cfg(unix)]
    #[test]
    fn a_graceful_child_runs_its_exit_trap_and_its_grace_is_a_bound() {
        let tmp = crate::mktemp_dir("atv-ceil-grace").expect("mktemp");
        let cases = [
            ("graced", Some(Duration::from_secs(600)), false, true),
            ("ungraced", None, false, false),
            (
                "ignores-term",
                Some(Duration::from_millis(700)),
                true,
                false,
            ),
        ];
        for (name, grace, ignore, trap_runs) in cases {
            let mut ceiling = Duration::from_millis(500);
            let (r, marker, grandchild) = loop {
                let stem = format!("{name}-{}ms", ceiling.as_millis());
                let (marker, pidfile) = (
                    tmp.join(format!("{stem}.trap")),
                    tmp.join(format!("{stem}.pid")),
                );
                let mut cmd = suite_with_trap(&marker, &pidfile, ignore);
                if let Some(g) = grace {
                    cmd = cmd.term_grace(g);
                }
                let r = run(&cmd, ceiled(&tmp, Some(ceiling)));
                if let Some(pid) = written_pid(&pidfile) {
                    break (r, marker, pid);
                }
                assert!(
                    ceiling < GONE,
                    "{name}: no shell backgrounded its sleep within {ceiling:?}"
                );
                ceiling *= 2;
            };
            assert!(
                !r.ok && r.output.contains(crate::findings::CEILING_KILL),
                "{name}: {}",
                r.output
            );
            assert_eq!(
                marker.exists(),
                trap_runs,
                "{name}: the EXIT trap\n{}",
                r.output
            );
            assert!(
                gone_within(&grandchild, GONE),
                "{name}: {grandchild} outlived the kill"
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// [`group::empty`] asks the kernel only about one real group: a pgid of 0 (to `killpg`, this
    /// process's own group), 1 (every process this user may signal), or one past `i32::MAX` is
    /// `None`, never asked; a group whose one member is this test's exited, unreaped child (a
    /// leader of its own session) is not empty (`0` on Linux, `EPERM` on macOS, which signals no
    /// zombie); and once the child is reaped the group is empty. NEGATIVE CONTROLS: let 0 or 1
    /// through its filter and each reads `Some(false)`; read `EPERM` as `None` and the zombie's
    /// group is unknown on macOS.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn only_a_real_group_is_asked_and_an_unreaped_leader_keeps_it() {
        let past = u32::try_from(i32::MAX).expect("i32::MAX") + 1;
        for pgid in [0, 1, past, u32::MAX] {
            assert_eq!(group::empty(pgid), None, "pgid {pgid}");
        }
        let mut leader = aterm_uds::spawnfd::command("/bin/sh");
        leader
            .args(["-c", "exit 0"])
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        let mut leader = leader.spawn().expect("a leader");
        let pid = leader.id();
        aterm_spec::wait::until_within(HANG_DETECTOR, "the leader's exit", || {
            group::exited(pid, false)
                .expect("its exit, unreaped")
                .then_some(())
        });
        assert_eq!(
            group::empty(pid),
            Some(false),
            "the unreaped leader keeps its group"
        );
        leader.wait().expect("the leader, reaped");
        assert_eq!(group::empty(pid), Some(true), "reaped, it holds no one");
    }

    /// A group whose members have all ended takes a signal without failing: on macOS its `killpg`
    /// answers `EPERM`, which [`aterm_uds::process::settle_group_signal`] settles from the listing,
    /// and a timed-out run must not report it as a group the kill failed to reach. NEGATIVE CONTROL:
    /// return `last_os_error` from `signal_group` unsettled and `kill` fails here on macOS.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_signal_to_a_group_whose_members_ended_is_not_a_failure() {
        let mut leader = aterm_uds::spawnfd::command("/bin/sh");
        leader
            .args(["-c", "exit 0"])
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        let mut leader = leader.spawn().expect("a leader");
        let pid = leader.id();
        aterm_spec::wait::until_within(HANG_DETECTOR, "the leader's exit", || {
            group::exited(pid, false)
                .expect("its exit, unreaped")
                .then_some(())
        });
        let live = group::Live::enter(pid);
        live.kill().expect("a group with nothing left to signal");
        live.term().expect("a group with nothing left to signal");
        drop(live);
        leader.wait().expect("the leader, reaped");
    }

    /// [`group::judged`] on each answer `killpg` gives: `ESRCH` is empty and `0` is not, neither
    /// reading the listing; any other errno is unknown; and `EPERM` is not empty, unless on Apple
    /// the group's whole listing finds no process, which is all an exec's leftover image holds
    /// there after its process is reaped. A listing cut short, none, or one that names a process
    /// (a zombie, another user's) keeps `EPERM` not empty. NEGATIVE CONTROL: answer
    /// `Some(false)` for every `EPERM` in `group::judged`, as it was, and the leftover's group
    /// reads not empty on Apple.
    #[cfg(unix)]
    #[test]
    fn a_group_held_only_by_an_exec_leftover_reads_empty() {
        use aterm_uds::process::GroupMembers;
        let unread = || -> Option<GroupMembers> { panic!("the listing is not read") };
        assert_eq!(group::judged(Err(Some(libc::ESRCH)), unread), Some(true));
        assert_eq!(group::judged(Ok(()), unread), Some(false));
        assert_eq!(group::judged(Err(Some(libc::EINVAL)), unread), None);
        assert_eq!(group::judged(Err(None), unread), None);
        let whole = GroupMembers {
            pids: Vec::new(),
            complete: true,
        };
        assert_eq!(
            group::judged(Err(Some(libc::EPERM)), || Some(whole.clone())),
            Some(cfg!(target_vendor = "apple")),
            "a whole listing that finds no process"
        );
        let cut = GroupMembers {
            pids: Vec::new(),
            complete: false,
        };
        let named = GroupMembers {
            pids: vec![4242],
            complete: true,
        };
        for listing in [Some(cut), Some(named), None] {
            assert_eq!(
                group::judged(Err(Some(libc::EPERM)), || listing.clone()),
                Some(false),
                "{listing:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_interrupted_gate_kills_every_live_stage_group() {
        use std::os::unix::process::ExitStatusExt as _;
        const PIDFILE: &str = "ATV_INTERRUPT_PIDFILE";
        if let Some(pidfile) = std::env::var_os(PIDFILE) {
            group::kill_on_interrupt();
            let pidfile = PathBuf::from(pidfile);
            let armed = pidfile.with_extension("armed");
            let ctx = crate::Ctx::new(
                PathBuf::from("/wt-a"),
                crate::Scope::workspace(),
                crate::EnvSnapshot::default(),
                PathBuf::from("/tmp/atv-s"),
            );
            let records = pidfile.with_extension("records");
            let record = crate::runs::Runs::open(&ctx, &records, |_, _| Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok: false,
                output: String::new(),
                code: None,
                spawn_error: None,
                usage: None,
            });
            if cfg!(any(target_vendor = "apple", target_os = "linux")) {
                assert!(record.is_some(), "the stand-in keeps a record");
            }
            std::thread::scope(|scope| {
                let stage = scope.spawn(|| {
                    run(
                        &with_grandchild(&pidfile),
                        ceiled(pidfile.parent().expect("a dir"), None),
                    )
                });
                let deadline = Instant::now() + HANG_DETECTOR;
                while !stage.is_finished() && Instant::now() < deadline {
                    if group::live_count() > 0 && written_pid(&pidfile).is_some() {
                        std::fs::write(&armed, "armed\n").expect("the armed marker");
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                let _ = stage.join();
            });
            group::end_interrupted();
            return;
        }
        let tmp = crate::mktemp_dir("atv-interrupt").expect("mktemp");
        let me = std::env::current_exe().expect("the test binary");
        let stand_in = |pidfile: &Path| {
            let mut c = std::process::Command::new(&me);
            c.args([
                "--exact",
                "exec::tests::an_interrupted_gate_kills_every_live_stage_group",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PIDFILE, pidfile)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
            let mut child = c.spawn().expect("the stand-in run starts");
            let armed = pidfile.with_extension("armed");
            let deadline = Instant::now() + HANG_DETECTOR;
            while !armed.exists() {
                if Instant::now() > deadline || matches!(child.try_wait(), Ok(Some(_))) {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(pid) = written_pid(pidfile) {
                        let _ = std::process::Command::new("/bin/kill")
                            .args(["-KILL", &pid])
                            .status();
                    }
                    panic!("the stand-in run never armed ({})", armed.display());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let grandchild = read_pid(pidfile);
            std::process::Command::new("/bin/kill")
                .args(["-INT", &child.id().to_string()])
                .status()
                .expect("/bin/kill runs");
            (child.wait().expect("the stand-in exits"), grandchild)
        };

        let (status, grandchild) = stand_in(&tmp.join("handled.pid"));
        assert_eq!(
            status.signal(),
            Some(2),
            "it still dies of SIGINT: {status:?}"
        );
        assert!(
            gone_within(&grandchild, GONE),
            "the grandchild {grandchild} outlived the interrupted run"
        );
        if cfg!(any(target_vendor = "apple", target_os = "linux")) {
            let kept = std::fs::read_dir(tmp.join("handled.records"))
                .map(|d| d.flatten().count())
                .unwrap_or(0);
            assert_eq!(kept, 1, "the interrupted run kept its record");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The lock [`lock_waiter_stand_in`] waits on.
    #[cfg(unix)]
    const WAIT_LOCK: &str = "ATERM_VERIFY_TEST_WAIT_LOCK";

    /// A complete fixture line, never a word in a command or checkout path.
    #[cfg(unix)]
    const LOCK_ADMITTED: &str = "ATERM_VERIFY_TEST_LOCK_ADMITTED";

    /// Set by [`lock_waiter_stand_in`]'s `SIGTERM` handler, as a lab's interrupt sets its stop.
    #[cfg(unix)]
    static STAND_IN_STOPPED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    #[cfg(unix)]
    extern "C" fn stand_in_stop(_: i32) {
        STAND_IN_STOPPED.store(true, Ordering::SeqCst);
    }

    /// How long [`lock_waiter_stand_in`] takes, once its wait is ended, to say NOT RUN, as a lab
    /// lets its claims go first.
    #[cfg(unix)]
    const STAND_IN_TEARDOWN: Duration = Duration::from_millis(500);

    /// Set, [`lock_waiter_stand_in`] waits at a test's pace: a line every tenth of a second, its
    /// holder called stuck after [`STUCK_AFTER`] of no progress.
    #[cfg(unix)]
    const WAIT_PACED: &str = "ATERM_VERIFY_TEST_LOCK_PACED";

    /// How long a paced [`lock_waiter_stand_in`] lets its holder make no progress before it calls
    /// it stuck.
    #[cfg(unix)]
    const STUCK_AFTER: Duration = Duration::from_secs(1);

    /// Run by the ceiling tests as a stage child: waits for the lock [`WAIT_LOCK`] names, as a lab
    /// waits for its lane, says when it has it, then works until it is ended or its run is gone.
    /// A `SIGTERM` ends its wait as a lab's interrupt does: NOT RUN, naming the holder, and exit 3.
    #[cfg(unix)]
    #[test]
    fn lock_waiter_stand_in() {
        use aterm_toolchain::named_lock::{Holder, Pace, Refused, Wait, take, take_paced};
        let Some(path) = std::env::var_os(WAIT_LOCK).map(PathBuf::from) else {
            return;
        };
        // SAFETY: `stand_in_stop` is an `extern "C" fn(i32)` that lives for the whole program and
        // only stores to an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGTERM, stand_in_stop as extern "C" fn(i32) as usize);
        }
        let me = Holder::me("lab linux exec", Path::new("/checkouts/waiter"));
        let stop = || STAND_IN_STOPPED.load(Ordering::SeqCst);
        let taken = if std::env::var_os(WAIT_PACED).is_some() {
            let pace = Pace {
                say_every: Duration::from_millis(100),
                stalled_after: STUCK_AFTER,
            };
            take_paced(&path, &me, Wait::Block, &mut |_, _| {}, &stop, pace)
        } else {
            take(&path, &me, Wait::Block, &mut |_, _| {}, &stop)
        };
        let _held = match taken {
            Ok(held) => held,
            Err(Refused::Stopped { holder, .. }) => {
                std::thread::sleep(STAND_IN_TEARDOWN);
                println!(
                    "NOT RUN: interrupted while waiting for {}, which holds the lane",
                    holder.map_or_else(|| "no one".to_string(), |h| h.describe(0))
                );
                std::process::exit(3);
            }
            Err(e) => panic!("the lock, once its holder lets go: {e:?}"),
        };
        println!("\n{LOCK_ADMITTED}");
        // Its run ends it; a run that died first (a failed test) left it a new parent, and it ends.
        let run = std::os::unix::process::parent_id();
        while std::os::unix::process::parent_id() == run {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A stand-in holder's lock at `dir/lane.lock`, and a stage child that waits on it.
    #[cfg(unix)]
    fn held_lane(dir: &Path) -> (aterm_toolchain::named_lock::NamedLock, Cmd) {
        held_lane_by(dir, std::process::id())
    }

    /// [`held_lane`], its record naming `pid` as the holder.
    #[cfg(unix)]
    fn held_lane_by(dir: &Path, pid: u32) -> (aterm_toolchain::named_lock::NamedLock, Cmd) {
        let lock = dir.join("lane.lock");
        let held = aterm_toolchain::named_lock::take(
            &lock,
            &aterm_toolchain::named_lock::Holder {
                pid,
                ..aterm_toolchain::named_lock::Holder::me(
                    "lab linux proof",
                    Path::new("/checkouts/holder"),
                )
            },
            aterm_toolchain::named_lock::Wait::No,
            &mut |_, _| {},
            &|| false,
        )
        .expect("a free lock");
        let waiter = Cmd::new(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "exec::tests::lock_waiter_stand_in",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(WAIT_LOCK, &lock);
        (held, waiter)
    }

    /// The wait the child of a run with `scratch` has marked, once it has: the one-minute hang
    /// detector. A mark this test laid ([`StartMark`]) is not the child's.
    #[cfg(unix)]
    fn marked(scratch: &Path) -> aterm_toolchain::waits::Waiting {
        let deadline = Instant::now() + HANG_DETECTOR;
        loop {
            let found = std::fs::read_dir(scratch)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("waits."))
                .find_map(|e| {
                    aterm_toolchain::waits::waiting(&e.path())
                        .into_iter()
                        .rfind(|w| w.pid != std::process::id())
                });
            if let Some(w) = found {
                return w;
            }
            assert!(Instant::now() < deadline, "the child never marked its wait");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A wait this test marks in the marks' directory of the one run with `scratch`, laid as soon
    /// as the run lays that directory (before or just after it spawns its child, the one-minute
    /// hang detector): the child's start, which its ceiling would count as work until the child
    /// marks a wait of its own, so a start a loaded machine holds back (an exec, a page-in) is
    /// not what its ceiling ends.
    #[cfg(unix)]
    struct StartMark {
        /// Held for its drop, which lets the mark go.
        _mark: aterm_toolchain::waits::Mark,
    }

    #[cfg(unix)]
    impl StartMark {
        fn lay(scratch: &Path) -> StartMark {
            let deadline = Instant::now() + HANG_DETECTOR;
            loop {
                let dir = std::fs::read_dir(scratch)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .find(|e| e.file_name().to_string_lossy().starts_with("waits."));
                if let Some(mark) = dir.and_then(|d| {
                    aterm_toolchain::waits::Mark::at(&d.path(), "the test's child, starting").ok()
                }) {
                    return StartMark { _mark: mark };
                }
                assert!(
                    Instant::now() < deadline,
                    "the run never laid its marks' directory"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// CEILING-WAIT's acceptance: a stage child waits on a lock its holder keeps for longer than
    /// the child's ceiling, and the ceiling does not end it while it waits; once it has the lock,
    /// the ceiling runs, and ends it at that much work, saying how long it waited apart. The
    /// child's start is marked a wait ([`StartMark`]) until its own wait is marked, so however
    /// late a loaded machine starts it, its ceiling times only the work after its admission.
    /// Negative control: timed from its spawn, as before, the ceiling ends it while it waits,
    /// never admitted, with no status.
    #[cfg(unix)]
    #[test]
    fn the_ceiling_times_a_childs_work_from_its_admission_not_its_wait() {
        let tmp = crate::mktemp_dir("atv-ceiling-wait").expect("mktemp");
        let (held, waiter) = held_lane(&tmp);
        let ceiling = Duration::from_secs(5);
        let running = {
            let tmp = tmp.clone();
            std::thread::spawn(move || run(&waiter, ceiled(&tmp, Some(ceiling))))
        };
        let start = StartMark::lay(&tmp);
        let wait = marked(&tmp);
        drop(start);
        assert!(
            wait.text.contains(&format!(
                "waiting for pid {} (lab linux proof, /checkouts/holder)",
                std::process::id()
            )),
            "{}",
            wait.text
        );
        // Hold the lane past the ceiling: the condition under test, not a wait for anything.
        std::thread::sleep(ceiling + Duration::from_secs(2));
        assert!(
            !running.is_finished(),
            "the ceiling ended the child while it waited: {:?}",
            running.join().map(|r| r.output)
        );
        drop(held);
        let out = running.join().expect("the run");
        assert!(!out.ok && out.code.is_none(), "{out:?}");
        assert!(
            out.output.lines().any(|line| line == LOCK_ADMITTED),
            "{}",
            out.output
        );
        assert!(
            out.output.contains(crate::findings::CEILING_KILL)
                && out.output.contains("of work, over the 5.0s ceiling")
                && out.output.contains("it also waited"),
            "{}",
            out.output
        );
        // A test run counts the same wait as work: a test stuck in one is a hang the ceiling ends.
        let lane = tmp.join("test-run");
        std::fs::create_dir(&lane).expect("a second lane");
        let (held, test) = held_lane(&lane);
        let r = run(
            &test.counting_waits(),
            ceiled(&tmp, Some(Duration::from_secs(2))),
        );
        assert!(
            r.output.contains(crate::findings::CEILING_KILL)
                && !r.output.lines().any(|line| line == LOCK_ADMITTED)
                && !r.output.contains("it also waited"),
            "{}",
            r.output
        );
        drop(held);
        // The Linux lane's row says so too, not a bare `no exit status`.
        let (_, row) = crate::stages::linux_lane_outcome(out.code, &out.output);
        assert!(
            row.contains("ended at the ceiling")
                && row.contains("of work, over the 5.0s ceiling")
                && row.contains("it also waited"),
            "{row}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A child the run's end finds waiting says so and names the holder, ended as an interrupt
    /// ends it: the run is marked ending first, then the child gets the handler's `SIGTERM`, which
    /// ends its wait, so its mark goes while it lives on to say NOT RUN. The run does not signal it
    /// again, so the child's own NOT RUN, naming the holder, is kept; the run's reason names the
    /// wait its clock last saw; and the Linux lane's row reads NOT RUN with it, the lane named and
    /// then the wait, never `killed by a signal, or never spawned`. Negative controls: a look
    /// after the end that forgets what the last one found reads `the run was interrupted while
    /// this child ran`; a run that ends the child itself cuts the child's NOT RUN; a row read from
    /// the exit code alone begins `no exit status — killed by a signal`.
    #[cfg(unix)]
    #[test]
    fn a_child_ended_while_it_waits_says_whom_it_waited_for() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let tmp = crate::mktemp_dir("atv-ended-waiting").expect("mktemp");
        let (held, waiter) = held_lane(&tmp);
        let (end, seen) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let running = {
            let (tmp, end, seen) = (tmp.clone(), Arc::clone(&end), Arc::clone(&seen));
            std::thread::spawn(move || {
                ends::when(end, seen);
                run(&waiter, ceiled(&tmp, Some(DEFAULT_CHILD_CEILING)))
            })
        };
        let wait = marked(&tmp);
        // The racing point, held: the run's clock has seen the wait before the end.
        let deadline = Instant::now() + HANG_DETECTOR;
        while !seen.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "the run's clock never saw the wait"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        end.store(true, Ordering::SeqCst);
        let pgid = i32::try_from(wait.pid).expect("a pid");
        // SAFETY: two integers; `pgid` is the run's own child, which leads its group and cannot be
        // reaped before it exits, and it waits on a lock this test holds until this signal ends
        // that wait.
        assert_eq!(unsafe { libc::killpg(pgid, libc::SIGTERM) }, 0);
        let out = running.join().expect("the run");
        drop(held);
        assert!(
            !out.output.lines().any(|line| line == LOCK_ADMITTED),
            "{}",
            out.output
        );
        assert!(
            out.output.contains(&format!(
                "NOT RUN: interrupted while waiting for pid {} (lab linux proof, /checkouts/holder",
                std::process::id()
            )),
            "the child was ended before it said why: {}",
            out.output
        );
        let why = out
            .spawn_error
            .clone()
            .expect("the run's end is its reason");
        assert!(
            why.starts_with("the run was interrupted while this child waited")
                && why.contains(&wait.text)
                && why.contains(&format!("pid {}", std::process::id())),
            "{why}"
        );
        let mut r = crate::Report::new("linux");
        r.record_driven(
            &out,
            crate::stages::LINUX_LANE,
            crate::stages::linux_lane_outcome,
        );
        let row = r.render();
        let row = row
            .lines()
            .find(|l| {
                l.starts_with(
                    crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun).tag(),
                )
            })
            .unwrap_or_else(|| panic!("no NOT RUN row: {row}"));
        // The row names the lane, then the wait: the child neither died of a signal of its own
        // nor failed to spawn, so it says neither.
        assert!(
            row.contains(&format!(
                "{} — the run was interrupted while this child waited",
                crate::stages::LINUX_LANE
            )) && row.contains(&wait.text),
            "{row}"
        );
        assert!(
            !row.contains("killed by a signal") && !row.contains("never spawned"),
            "{row}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A wait its waiter calls stuck ends the child only on a second look in a row: a mark is
    /// rewritten in place, and a read between its new record's write and its truncation finds
    /// the replaced record's `stuck` line after the new first line, as the record here is laid.
    /// A holder in the child's own group is never the reason, however long the wait is stuck.
    /// Negative control: a clock that believes one look ends the child on the torn record.
    #[cfg(unix)]
    #[test]
    fn a_stuck_wait_is_believed_on_its_second_look() {
        use aterm_toolchain::waits::{Mark, Stuck};
        let tmp = crate::mktemp_dir("atv-stuck-looks").expect("mktemp");
        let marks = tmp.join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        // The holder: this process, whose group is this test's own.
        let holder = std::process::id();
        // SAFETY: `getpgid(0)` reads this process's group; it signals nothing.
        let group = u32::try_from(unsafe { libc::getpgid(0) }).expect("this process's group");
        let stuck = Stuck {
            holders: vec![holder],
            span: Duration::from_secs(900),
        };
        let mut mark = Mark::at(&marks, "waiting for its holder, which works").expect("a mark");
        let mut clock = Clock::new(Some(&marks), &INERT, std::process::id(), None);
        // The torn record: the new first line, the replaced record's `stuck` line after it.
        let path = std::fs::read_dir(&marks)
            .expect("the marks")
            .flatten()
            .map(|e| e.path())
            .next()
            .expect("the mark's file");
        let torn =
            format!("{holder}\t1\twaiting for its holder, which works\nstuck\t900\t{holder}\n");
        std::fs::write(&path, &torn).expect("the torn record");
        clock.look();
        assert!(
            clock.waits.iter().any(|w| w.stuck.is_some()),
            "{:?}",
            clock.waits
        );
        assert_eq!(clock.stuck(u32::MAX), None, "one look");
        mark.say("waiting for its holder, which works");
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "the record said anew");
        mark.say_stuck(
            "waiting for its holder, which has made no progress",
            Some(&stuck),
        );
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "its first look");
        clock.look();
        let why = clock.stuck(u32::MAX).expect("its second look");
        assert!(
            why.contains("no progress for 15m00s")
                && why.contains("waiting for its holder, which has made no progress"),
            "{why}"
        );
        assert_eq!(clock.stuck(group), None, "a holder in the child's group");
        drop(mark);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A second stuck mark cannot borrow the first mark's confirmation. Dropping the mark or
    /// observing it without a stuck tail also ends its confirmation, while fresh diagnostic
    /// text and a growing elapsed span on the same wait do not.
    #[cfg(unix)]
    #[test]
    fn stuck_confirmation_belongs_to_the_same_live_mark() {
        use aterm_toolchain::waits::{Mark, Stuck};
        let tmp = crate::mktemp_dir("atv-stuck-mark").expect("mktemp");
        let marks = tmp.join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        let holder = std::process::id();
        let stuck = Stuck {
            holders: vec![holder],
            span: Duration::from_secs(900),
        };
        let mut first = Mark::at(&marks, "waiting").expect("first mark");
        let mut second = Mark::at(&marks, "waiting").expect("second mark");
        let mut clock = Clock::new(Some(&marks), &INERT, holder, None);
        first.say_stuck("waiting", Some(&stuck));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "first mark's first look");
        first.say("waiting");
        second.say_stuck("waiting", Some(&stuck));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "second mark's first look");
        second.say_stuck(
            "diagnostic changed",
            Some(&Stuck {
                span: Duration::from_secs(901),
                ..stuck.clone()
            }),
        );
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some(), "same mark's second look");
        second.say("waiting");
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "not stuck this look");
        second.say_stuck("waiting", Some(&stuck));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "confirmation must start again");
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some());
        drop(first);
        drop(second);
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "no live mark");
        let mut third = Mark::at(&marks, "waiting").expect("third mark");
        third.say_stuck("waiting", Some(&stuck));
        clock.look();
        assert_eq!(
            clock.stuck(u32::MAX),
            None,
            "empty look cleared confirmation"
        );
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some());
        drop(third);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A wait's replacement or expanded holder set needs its own second look. Ordering,
    /// duplicates, diagnostic words and a growing span are not changes to that holder set.
    #[cfg(unix)]
    #[test]
    fn stuck_confirmation_belongs_to_the_same_holder_set() {
        use aterm_toolchain::waits::{Mark, Stuck};
        let tmp = crate::mktemp_dir("atv-stuck-holders").expect("mktemp");
        let marks = tmp.join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        let holder = std::process::id();
        let other = u32::MAX - 1;
        let mut mark = Mark::at(&marks, "waiting").expect("a mark");
        let mut clock = Clock::new(Some(&marks), &INERT, holder, None);
        let mut say = |holders, span| {
            mark.say_stuck("waiting", Some(&Stuck { holders, span }));
        };
        say(vec![holder], Duration::from_secs(900));
        clock.look();
        say(vec![other], Duration::from_secs(900));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "a different holder");
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some(), "holder confirmed");
        say(vec![holder, other], Duration::from_secs(901));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "a new holder joined");
        say(vec![other, holder, holder], Duration::from_secs(902));
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some(), "same normalized set");
        say(vec![holder], Duration::from_secs(903));
        clock.look();
        assert_eq!(clock.stuck(u32::MAX), None, "one holder left");
        clock.look();
        assert!(clock.stuck(u32::MAX).is_some());
        drop(mark);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// An established wait inside the child's group cannot prime a new external wait. This
    /// checks selection when a confirmed but excluded record remains beside the new record.
    #[cfg(unix)]
    #[test]
    fn an_excluded_mark_cannot_confirm_a_new_external_wait() {
        use aterm_toolchain::waits::{Mark, Stuck};
        let tmp = crate::mktemp_dir("atv-stuck-excluded").expect("mktemp");
        let marks = tmp.join("waits");
        std::fs::create_dir(&marks).expect("the marks' dir");
        let holder = std::process::id();
        // SAFETY: `getpgid(0)` reads this process's group; it signals nothing.
        let group = u32::try_from(unsafe { libc::getpgid(0) }).expect("this process's group");
        let mut internal = Mark::at(&marks, "internal wait").expect("internal mark");
        internal.say_stuck(
            "internal wait",
            Some(&Stuck {
                holders: vec![holder],
                span: Duration::from_secs(900),
            }),
        );
        let mut clock = Clock::new(Some(&marks), &INERT, holder, None);
        clock.look();
        clock.look();
        assert_eq!(clock.stuck(group), None, "its holder would be signalled");
        let mut external = Mark::at(&marks, "external wait").expect("external mark");
        // A synthetic PID outside any representable process group, not a process to signal.
        external.say_stuck(
            "external wait",
            Some(&Stuck {
                holders: vec![u32::MAX - 1],
                span: Duration::from_secs(900),
            }),
        );
        clock.look();
        assert_eq!(clock.stuck(group), None, "new external wait seen only once");
        clock.look();
        assert!(clock.stuck(group).is_some(), "external wait seen twice");
        drop(internal);
        drop(external);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Set, the test binary is the idle [`holder_stand_in`].
    #[cfg(unix)]
    const HOLDER_STAND_IN: &str = "ATERM_VERIFY_TEST_HOLDER";

    /// Run as a lock's named holder by [`a_child_waiting_behind_a_stuck_holder_could_not_run`]:
    /// lives for twice the hang detector unless its test ends it first, using no CPU time and
    /// writing nothing.
    #[cfg(unix)]
    #[test]
    fn holder_stand_in() {
        let Some(_) = std::env::var_os(HOLDER_STAND_IN) else {
            return;
        };
        std::thread::sleep(HANG_DETECTOR * 2);
    }

    /// A [`holder_stand_in`], killed when dropped: no test leaves one behind.
    #[cfg(unix)]
    struct HolderStandIn(aterm_uds::spawnfd::Child);

    #[cfg(unix)]
    impl HolderStandIn {
        fn start() -> HolderStandIn {
            HolderStandIn(
                aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
                    .args([
                        "--exact",
                        "exec::tests::holder_stand_in",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(HOLDER_STAND_IN, "1")
                    .stdin(aterm_uds::spawnfd::Stdio::null())
                    .stdout(aterm_uds::spawnfd::Stdio::null())
                    .stderr(aterm_uds::spawnfd::Stdio::null())
                    .spawn()
                    .expect("the holder stand-in"),
            )
        }

        /// Whether it still runs: no signal reached it.
        fn runs(&mut self) -> bool {
            matches!(self.0.try_wait(), Ok(None))
        }
    }

    #[cfg(unix)]
    impl Drop for HolderStandIn {
        fn drop(&mut self) {
            if self.runs() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    /// The whole seconds an [`aterm_capacity::span`] under an hour reads (`7s`, `2m05s`).
    #[cfg(unix)]
    fn span_secs(span: &str) -> Option<u64> {
        let secs = span.strip_suffix('s')?;
        match secs.split_once('m') {
            Some((m, s)) => Some(m.parse::<u64>().ok()? * 60 + s.parse::<u64>().ok()?),
            None => secs.parse().ok(),
        }
    }

    /// CW-WAIT-UNBOUNDED: a stage child waits on a lock whose named holder lives on and makes no
    /// progress. Once its waiter calls the holder stuck, the run ends the child (`SIGTERM`, so it
    /// says its own NOT RUN within [`STUCK_GRACE`]) rather than waiting on, inside the hang
    /// detector past that grace and with no ceiling that could end it instead; the child could
    /// not run, naming the holder and how long it made no progress, at least the waiter's bound;
    /// the Linux lane's row reads NOT RUN with it; and the holder is never signalled and still
    /// runs. Working-holder classification is covered by the counter-driven tests in
    /// `aterm_toolchain::stall`, including a runnable child receiving only a twelfth of a core.
    /// No CPU scheduling share is required within a real one-second observation window here.
    /// The run has no ceiling, so a starved child start cannot decide the result.
    #[cfg(unix)]
    #[test]
    fn a_child_waiting_behind_a_stuck_holder_could_not_run() {
        let tmp = crate::mktemp_dir("atv-stuck-holder").expect("mktemp");
        let mut holder = HolderStandIn::start();
        let (held, waiter) = held_lane_by(&tmp, holder.0.id());
        let waiter = waiter.env(WAIT_PACED, "1");
        let started = Instant::now();
        let running = {
            let (tmp, waiter) = (tmp.clone(), waiter.clone());
            std::thread::spawn(move || run(&waiter, ceiled(&tmp, None)))
        };
        aterm_spec::wait::until_within(
            STUCK_GRACE + HANG_DETECTOR,
            "the run to end the child",
            || running.is_finished().then_some(()),
        );
        let out = running.join().expect("the run");
        eprintln!(
            "the stuck holder ended the child {:?} after it began",
            started.elapsed()
        );
        let why = out
            .spawn_error
            .clone()
            .expect("the stuck holder is its reason");
        let named = format!("pid {} (lab linux proof, /checkouts/holder)", holder.0.id());
        let figure = why
            .strip_prefix(
                "this child waited for admission or a lock behind a holder that has made no \
                 progress for ",
            )
            .and_then(|rest| rest.split_once(", so it was ended"))
            .and_then(|(span, _)| span_secs(span));
        assert!(
            figure.is_some_and(|secs| secs >= STUCK_AFTER.as_secs())
                && why.contains(&format!("{named}, which has held"))
                && why.contains(", and which has made no progress for "),
            "{why}"
        );
        assert!(
            out.output.contains(&format!(
                "NOT RUN: interrupted while waiting for {}",
                named.trim_end_matches(')')
            )),
            "the child was ended before it said why: {}",
            out.output
        );
        assert!(
            !out.output.lines().any(|l| l == LOCK_ADMITTED),
            "{}",
            out.output
        );
        let mut r = crate::Report::new("linux");
        r.record_driven(
            &out,
            crate::stages::LINUX_LANE,
            crate::stages::linux_lane_outcome,
        );
        let row = r.render();
        assert!(
            row.lines().any(|l| l.starts_with(
                crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun).tag()
            ) && l.contains(&format!(
                "{} — this child waited for admission or a lock behind a holder",
                crate::stages::LINUX_LANE
            )) && l.contains(&named)),
            "{row}"
        );
        assert!(holder.runs(), "the holder was signalled");
        assert_eq!(
            aterm_toolchain::named_lock::read(&tmp.join("lane.lock")).map(|h| h.pid),
            Some(holder.0.id()),
            "the holder keeps the lock"
        );
        drop((held, holder));

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A host whose every read of a group finds one member, the group's leader, whose counters
    /// have grown by 1 ms of CPU, 2 ms runnable and 1 MiB held at each read so far, whatever
    /// the group: how many reads a child gets is the clock's affair, and its last is its most.
    #[cfg(unix)]
    #[derive(Debug, Default)]
    struct Counting {
        reads: AtomicU64,
    }

    #[cfg(unix)]
    impl Host for Counting {
        fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
            let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            Some(vec![aterm_sysprobe::MemberUsage {
                pid: pgid,
                start: 1,
                cpu_ns: n * 1_000_000,
                runnable_ns: n * 2_000_000,
                footprint: n << 20,
                peak_footprint: n << 20,
                ..aterm_sysprobe::MemberUsage::default()
            }])
        }

        fn idle(&self) -> Option<Duration> {
            None
        }

        fn cores(&self) -> u32 {
            1
        }
    }

    /// A STAGE'S CHILDREN SUM TO THEIR STAGE (A1): two children run one after the other under one
    /// stage's meter each return the use their own reads found, booked on exactly the reads that
    /// book the stage (the clock's, then the sweep's of the ended leader, the last and most), so
    /// their CPU sums to the stage's observed CPU to the nanosecond, whatever number of reads each
    /// got. Each child's CPU is its last read's, its runnable time twice that, its peak that
    /// read's footprint. NEGATIVE CONTROL: a sweep that books the stage's meter and not the
    /// child's own leaves each child its second-to-last read, and the sum falls short by 2 ms.
    #[cfg(unix)]
    #[test]
    fn a_stages_children_sum_to_their_stage() {
        let tmp = crate::mktemp_dir("atv-usage-sum").expect("mktemp");
        let host: &'static Counting = Box::leak(Box::default());
        let stage = crate::memory::Meter::new();
        let _metering = crate::memory::metering(Some(Arc::clone(&stage)));
        let env = ExecEnv {
            host,
            ..env_in(&tmp)
        };
        let child = || {
            let out = run(&Cmd::new("/bin/sh").args(["-c", "exit 0"]), env);
            assert!(out.ok, "{out:?}");
            (
                out.usage.expect("its tree was read"),
                host.reads.load(Ordering::SeqCst),
            )
        };
        let (a, after_a) = child();
        let (b, after_b) = child();
        let ms = Duration::from_millis;
        assert_eq!(
            a.cpu,
            ms(after_a),
            "the first child's last read is its most"
        );
        assert_eq!(b.cpu, ms(after_b), "{b:?}");
        assert_eq!(
            (a.runnable, a.peak_bytes, a.processes),
            (ms(2 * after_a), after_a << 20, 1)
        );
        let observed = stage.observed_cpu().expect("the stage was read");
        assert_eq!(
            a.cpu + b.cpu,
            Duration::from_nanos(observed.nanoseconds),
            "the children sum to their stage"
        );
        assert_eq!(a.processes + b.processes, observed.processes);
        assert!(a.wall > Duration::ZERO && b.wall > Duration::ZERO);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A host with no reading of a child's tree (every platform but macOS) gives its run no
    /// usage, never zeros; nor does a child that never started. NEGATIVE CONTROL: the same child
    /// on a host that reads its tree has usage.
    #[cfg(unix)]
    #[test]
    fn a_host_with_no_reading_gives_a_run_no_usage() {
        let tmp = crate::mktemp_dir("atv-usage-none").expect("mktemp");
        let child = Cmd::new("/bin/sh").args(["-c", "exit 3"]);
        let out = run(&child, env_in(&tmp));
        assert_eq!((out.ok, out.code, out.usage), (false, Some(3), None));
        let missing = run(&Cmd::new("/no/such/tool"), env_in(&tmp));
        assert!(missing.spawn_error.is_some() && missing.usage.is_none());
        let host: &'static Counting = Box::leak(Box::default());
        let read = run(
            &child,
            ExecEnv {
                host,
                ..env_in(&tmp)
            },
        );
        assert!(
            read.usage.is_some_and(|u| u.cpu > Duration::ZERO),
            "{read:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A stage child that writes its pid to `pidfile` (its shell leads its group), then runs
    /// `then`.
    #[cfg(unix)]
    fn stand_in(pidfile: &Path, then: &str) -> Cmd {
        Cmd::new("/bin/sh").args(["-c", &format!("echo $$ > '{}'; {then}", pidfile.display())])
    }

    /// A child [`running`] on a thread of its own, and where its run and the time it ended come.
    #[cfg(unix)]
    type Running = (
        std::thread::JoinHandle<()>,
        std::sync::mpsc::Receiver<(Run, Instant)>,
    );

    /// `cmd` run on a thread of its own under `ceiling` on `host`.
    #[cfg(unix)]
    fn running(cmd: Cmd, dir: &Path, ceiling: Duration, host: &'static dyn Host) -> Running {
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = dir.to_path_buf();
        let thread = std::thread::spawn(move || {
            let env = ExecEnv {
                host,
                ..ceiled(&dir, Some(ceiling))
            };
            let out = run(&cmd, env);
            let _ = tx.send((out, Instant::now()));
        });
        (thread, rx)
    }

    /// The run `running` answers, once its ceiling ends its child ([`ended_at`]).
    #[cfg(unix)]
    fn ended(running: Running, pidfile: &Path, what: &str) -> Run {
        ended_at(running, pidfile, what).0
    }

    /// The run `rx` answers and when it ended, once its ceiling ends its child: the one-minute
    /// hang detector, past which the child's group (by the pid it wrote) is killed and the test
    /// fails.
    #[cfg(unix)]
    fn ended_at((thread, rx): Running, pidfile: &Path, what: &str) -> (Run, Instant) {
        let Ok(out) = rx.recv_timeout(HANG_DETECTOR) else {
            if let Some(pid) = written_pid(pidfile) {
                let _ = std::process::Command::new("/bin/kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .status();
            }
            let _ = thread.join();
            panic!("{what}: the ceiling never ended it");
        };
        let _ = thread.join();
        out
    }

    /// What a [`HeldAt`] host counts: its answers to `unstarted`, `daemon` and `backlog`.
    #[cfg(unix)]
    #[derive(Clone, Copy, Debug)]
    enum Asked {
        Unstarted,
        Daemon,
        Backlog,
    }

    /// A stand-in host for a child's tree held at macOS's program check: the child's group as one
    /// member with no CPU and no I/O, beside `member` when that is a deeper one. Its clock reads
    /// `made_us` at a watch's first look and moves `step` at each look after (each `unstarted`),
    /// so a test spans the minutes a syspolicyd's grace takes in a few seconds. The held one forked
    /// `held` before `made_us` (`None`: nothing unrun), reads unrun from look `unrun_from` on and,
    /// when `runs`, at another CPU at each look (it ran between them), and when `stopped`, stopped
    /// as a start-suspended spawn reads (`StoppedUnstarted`, never held); it waits under the
    /// syspolicyd `daemon` (pid [`HeldAt::DAEMON`]) and the backlog it is told. It counts what it
    /// is asked, and keeps the child's pgid.
    #[cfg(unix)]
    #[derive(Debug)]
    struct HeldAt {
        made_us: u64,
        step: Duration,
        held: Option<Duration>,
        unrun_from: u64,
        runs: bool,
        stopped: bool,
        /// A deeper member's pid, executable and chain; `None` holds the leader.
        member: Option<(u32, PathBuf, Option<Vec<PathBuf>>)>,
        daemon: DaemonNow,
        backlog: Backlog,
        asked: [AtomicU64; 3],
        pgid: AtomicU64,
        release_on_second_look: std::sync::Mutex<Option<File>>,
        clock: Option<(PathBuf, Instant, AtomicU64)>,
    }

    #[cfg(unix)]
    impl HeldAt {
        const DAEMON: u32 = 4242;

        /// A leader held `held` before the first look, under a syspolicyd that started
        /// `daemon_at` seconds after that look (before it, when negative; `None`: none runs), its
        /// pool read `pool_full` and `held_since` held programs forked since it started; its
        /// clock moves 11 min a look, past the daemon's grace by the second.
        fn leader(
            held: Duration,
            daemon_at: Option<i64>,
            pool_full: Option<bool>,
            held_since: Option<usize>,
        ) -> HeldAt {
            let made_us = aterm_sysprobe::jobs::now_us();
            let daemon = daemon_at.map_or(DaemonNow::NoneRunning, |at| {
                let s = made_us
                    .checked_add_signed(at * 1_000_000)
                    .expect("in range");
                DaemonNow::Running(aterm_uds::program_check::Daemon {
                    pid: Self::DAEMON,
                    start_s: s / 1_000_000,
                    start_us: u32::try_from(s % 1_000_000).expect("under a second"),
                })
            });
            HeldAt {
                made_us,
                step: Duration::from_secs(11 * 60),
                held: Some(held),
                unrun_from: 0,
                runs: false,
                stopped: false,
                member: None,
                daemon,
                backlog: Backlog {
                    pool_full,
                    held_since,
                },
                asked: Default::default(),
                pgid: AtomicU64::new(0),
                release_on_second_look: Default::default(),
                clock: None,
            }
        }

        /// The stranded shape (2026-10-29's band): held 31 min, seen held by the first look, under
        /// a syspolicyd that started a second after it and answers alone.
        fn stranded() -> HeldAt {
            Self::leader(Duration::from_secs(31 * 60), Some(1), Some(false), Some(0))
        }

        fn leaked(self) -> &'static HeldAt {
            Box::leak(Box::new(self))
        }

        /// Advance only after the actual shell wrote its execution proof. Until then the
        /// clock stays at its base; the parent's one-minute hang detector bounds that wait.
        fn advancing_after(mut self, proof: PathBuf) -> Self {
            self.clock = Some((proof, Instant::now(), AtomicU64::new(0)));
            self
        }

        fn asked(&self, what: Asked) -> u64 {
            self.asked[what as usize].load(Ordering::SeqCst)
        }

        fn ask(&self, what: Asked) -> u64 {
            self.asked[what as usize].fetch_add(1, Ordering::SeqCst)
        }

        /// The held one's pid in group `pgid`.
        fn held_pid(&self, pgid: u32) -> u32 {
            self.member.as_ref().map_or(pgid, |(pid, _, _)| *pid)
        }
    }

    #[cfg(unix)]
    impl Host for HeldAt {
        fn monotonic(&self) -> Instant {
            match &self.clock {
                Some((proof, began, ticks)) => {
                    let ran = std::fs::read_to_string(proof)
                        .ok()
                        .and_then(|line| line.trim().parse::<u32>().ok())
                        .is_some_and(|pid| pid > 0);
                    let elapsed = if ran {
                        ticks.fetch_add(100, Ordering::SeqCst)
                    } else {
                        0
                    };
                    *began + Duration::from_millis(elapsed)
                }
                None => Instant::now(),
            }
        }
        fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
            self.pgid.store(u64::from(pgid), Ordering::SeqCst);
            let held = self.held_pid(pgid);
            let cpu = if self.runs {
                1000 * self.asked(Asked::Unstarted)
            } else {
                0
            };
            let quiet = |pid| aterm_sysprobe::MemberUsage {
                pid,
                start: 1,
                cpu_ns: if pid == held { cpu } else { 0 },
                ..aterm_sysprobe::MemberUsage::default()
            };
            Some(
                std::iter::once(pgid)
                    .chain(self.member.as_ref().map(|(pid, _, _)| *pid))
                    .map(quiet)
                    .collect(),
            )
        }

        fn idle(&self) -> Option<Duration> {
            None
        }

        fn cores(&self) -> u32 {
            1
        }

        fn now_us(&self) -> u64 {
            let looks = u32::try_from(self.asked(Asked::Unstarted)).expect("few looks");
            self.made_us + u64::try_from((self.step * looks).as_micros()).expect("small")
        }

        fn unstarted(&self, pgid: u32, pids: &[u32], now_us: u64) -> Vec<Unstarted> {
            let look = self.ask(Asked::Unstarted);
            if look == 1
                && let Some(release) = self.release_on_second_look.lock().unwrap().as_mut()
            {
                release.write_all(b"observed\n").expect("release the child");
            }
            let Some(held) = self.held else {
                return Vec::new();
            };
            let (pid, exe, chain) = match &self.member {
                None => (pgid, None, Some(Vec::new())),
                Some((pid, exe, chain)) => (*pid, Some(exe.clone()), chain.clone()),
            };
            if look < self.unrun_from || !pids.contains(&pid) {
                return Vec::new();
            }
            let start_us = self.made_us - u64::try_from(held.as_micros()).expect("small");
            vec![Unstarted {
                pid,
                facts: aterm_sysprobe::jobs::StartFacts {
                    lone_zero: true,
                    user_ns: Some(0),
                    program_check_wait: true,
                    stopped: self.stopped,
                    local: Some(true),
                    age_ms: Some(now_us.saturating_sub(start_us) / 1000),
                    start_us: Some(start_us),
                    ..aterm_sysprobe::jobs::StartFacts::default()
                },
                exe,
                chain,
            }]
        }

        fn daemon(&self) -> DaemonNow {
            self.ask(Asked::Daemon);
            self.daemon
        }

        fn backlog(&self, _: aterm_uds::program_check::Daemon, _: u64) -> Backlog {
            self.ask(Asked::Backlog);
            self.backlog
        }
    }

    /// 2026-10-29's band: a stage child seen held at its first exec before the syspolicyd running
    /// now started, which has then run past the bound on a first exec and answers alone, ends
    /// within the hang detector under a 3 h ceiling: unrun, naming its pid and the daemon's, a
    /// COULD NOT RUN row and no ceiling finding, its group gone. Negative controls: the ones
    /// beside it (a daemon its check may still reach) wait to their ceiling.
    #[cfg(unix)]
    #[test]
    fn a_child_held_since_before_the_running_syspolicyd_ends_could_not_run_within_a_minute() {
        let tmp = crate::mktemp_dir("atv-stranded").expect("mktemp");
        let pidfile = tmp.join("stranded.pid");
        let host = HeldAt::stranded().leaked();
        let out = ended(
            running(
                stand_in(&pidfile, "exec sleep 600"),
                &tmp,
                DEFAULT_CHILD_CEILING,
                host,
            ),
            &pidfile,
            "a child its syspolicyd stranded",
        );
        let pid = host.pgid.load(Ordering::SeqCst);
        assert!(!out.ok && out.code.is_none(), "{out:?}");
        let why = out.spawn_error.as_deref().expect("ended unrun");
        assert!(
            why.contains(&format!("(pid {pid})"))
                && why.contains(&format!("(pid {})", HeldAt::DAEMON)),
            "{why}"
        );
        assert_eq!(out.environment_failure().as_deref(), Some(why));
        assert!(
            !out.output.contains(crate::findings::CEILING_KILL),
            "{}",
            out.output
        );
        let mut r = crate::Report::new("stranded");
        r.decide_child(&out, "band");
        assert!(
            r.outcomes()
                .all(|(o, _)| o == crate::Outcome::Fail(crate::Severity::CouldNotRun)),
            "{}",
            r.render()
        );
        assert!(gone_within(&pid.to_string(), HANG_DETECTOR), "pid {pid}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The work clock must use its injected monotonic source even after usage disappears.
    /// Replacing it with Instant::now loses the planted hour and fails this assertion.
    #[cfg(unix)]
    #[test]
    fn the_work_clock_advances_from_its_host_without_sleeping() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let proof = dir.path().join("proof");
        std::fs::write(&proof, b"42\n").unwrap();
        let host = HeldAt::stranded().advancing_after(proof);
        let mut clock = Clock::new(None, &host, 1, None);
        clock.usage = None;
        host.clock
            .as_ref()
            .unwrap()
            .2
            .store(3_600_000, Ordering::SeqCst);
        clock.tick();
        assert!(clock.worked >= Duration::from_secs(3599));
    }

    /// Negative controls for the stranded rule, each a leader held 31 min under a 9 s ceiling,
    /// over which its watch looks twice, 11 min apart on its host's clock: a syspolicyd older than
    /// its first look (its check went to the one running now); one that started after it but has
    /// run only 9 min by the second, short of the bound on a first exec (a check queued for it
    /// may still be answered); a full pool, an unread pool, a backlog (a held program forked
    /// since that daemon started); no syspolicyd and an unread one; a leader first read unrun at
    /// the second look (its fork predates the daemon, but its exec, in the same pid, does not);
    /// one whose CPU moved between the looks (it ran, and may have exec'd again); a leader that
    /// reads stopped before its first instruction (`StoppedUnstarted`, never held, so the daemon
    /// is never asked of it). Each waits to its ceiling and ends there unrun, not as a finding,
    /// with the leader still unstarted, its words saying how. The deadline clock advances only
    /// after the actual child wrote its pid, so these cases do not wait nine real seconds.
    /// The backlog is read only once the
    /// leader's check was sent before a daemon past its grace. A tree with nothing unrun ends at
    /// its ceiling as the finding it was.
    #[cfg(unix)]
    #[test]
    fn a_held_child_its_syspolicyd_may_still_answer_is_waited_for_to_its_ceiling() {
        let tmp = crate::mktemp_dir("atv-held-waits").expect("mktemp");
        let ceiling = Duration::from_secs(9);
        let held = Duration::from_secs(31 * 60);
        let leader = |at, pool_full, held_since| HeldAt::leader(held, at, pool_full, held_since);
        let mut in_grace = HeldAt::stranded();
        in_grace.step = Duration::from_secs(9 * 60);
        let mut unread = HeldAt::stranded();
        unread.daemon = DaemonNow::Unread;
        let mut exec_later = HeldAt::stranded();
        exec_later.unrun_from = 1;
        let mut ran = HeldAt::stranded();
        ran.runs = true;
        let mut nothing = HeldAt::stranded();
        nothing.held = None;
        let mut stopped = HeldAt::stranded();
        stopped.stopped = true;
        let cases = [
            (
                "an older daemon",
                leader(Some(-32 * 60), Some(false), Some(0)),
                false,
            ),
            ("a daemon short of its grace", in_grace, false),
            ("a full pool", leader(Some(1), Some(true), Some(0)), true),
            ("an unread pool", leader(Some(1), None, Some(0)), true),
            ("a backlog", leader(Some(1), Some(false), Some(1)), true),
            ("no daemon", leader(None, Some(false), Some(0)), false),
            ("an unread daemon", unread, false),
            ("an exec after the daemon started", exec_later, false),
            ("a CPU that moved", ran, false),
            ("a leader stopped before it ran", stopped, false),
            ("nothing unrun", nothing, false),
        ]
        .map(|(what, host, backlog)| {
            let pidfile = tmp.join(format!("{what}.pid"));
            let host = host.advancing_after(pidfile.clone()).leaked();
            let run = running(stand_in(&pidfile, "exec sleep 600"), &tmp, ceiling, host);
            (what, host, backlog, pidfile, run)
        });
        for (what, host, backlog, pidfile, run) in cases {
            let out = ended(run, &pidfile, what);
            assert!(
                u128::from(host.clock.as_ref().unwrap().2.load(Ordering::SeqCst))
                    >= ceiling.as_millis(),
                "{what}: ended before its ceiling: {out:?}"
            );
            assert!(!out.ok && out.code.is_none(), "{what}: {out:?}");
            assert!(host.asked(Asked::Unstarted) >= 3, "{what}: {host:?}");
            if host.held.is_none() {
                assert!(
                    out.spawn_error.is_none() && out.output.contains(crate::findings::CEILING_KILL),
                    "{what}: {out:?}"
                );
                continue;
            }
            let why = out
                .spawn_error
                .as_deref()
                .expect("ended unrun at its ceiling");
            assert!(!why.contains("stranded"), "{what}: {why}");
            assert_eq!(
                why.contains("it read stopped"),
                host.stopped,
                "{what}: {why}"
            );
            if host.stopped {
                assert_eq!(host.asked(Asked::Daemon), 0, "{what}: never held");
            }
            assert!(
                !out.output.contains(crate::findings::CEILING_KILL),
                "{what}: {}",
                out.output
            );
            assert_eq!(
                host.asked(Asked::Backlog) > 0,
                backlog,
                "{what}: backlog asked {} times",
                host.asked(Asked::Backlog)
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The slow, healthy first exec: a leader held 9 min 59 s at the first look, seen held past
    /// the 10 min line by the second, under a syspolicyd older than it whose pool answers, runs
    /// on and passes. Its start is asked again, its backlog never.
    #[cfg(unix)]
    #[test]
    fn a_slow_first_exec_under_the_running_syspolicyd_runs_on_and_passes() {
        let tmp = crate::mktemp_dir("atv-slow-exec").expect("mktemp");
        let pidfile = tmp.join("slow.pid");
        let fifo = tmp.join("observed.fifo");
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `path` owns the NUL-terminated name throughout the call.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        // Opening both ends cannot block on a child that has not started yet. The release line
        // remains buffered until that child reads it, however late it starts.
        let release = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&fifo)
            .unwrap();
        let host = HeldAt::leader(
            Duration::from_secs(10 * 60 - 1),
            Some(-20 * 60),
            Some(false),
            Some(0),
        );
        *host.release_on_second_look.lock().unwrap() = Some(release);
        let host = host.leaked();
        let out = ended(
            running(
                stand_in(&pidfile, &format!("read observed < '{}'", fifo.display())),
                &tmp,
                DEFAULT_CHILD_CEILING,
                host,
            ),
            &pidfile,
            "a slow first exec",
        );
        assert!(out.ok && out.spawn_error.is_none(), "{out:?}");
        assert!(host.asked(Asked::Unstarted) >= 2, "{host:?}");
        assert_eq!(host.asked(Asked::Backlog), 0, "{host:?}");
        host.release_on_second_look.lock().unwrap().take();
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A held deeper member, as cargo's `build-script-build` under `cargo` and `targo`.
    #[cfg(unix)]
    fn member_of(mut host: HeldAt, chain: &[&str]) -> &'static HeldAt {
        host.member = Some((
            999_999,
            PathBuf::from("/r/target/debug/build/x-1/build-script-build"),
            Some(chain.iter().map(PathBuf::from).collect()),
        ));
        host.leaked()
    }

    #[cfg(unix)]
    const UNDER_CARGO: [&str; 2] = ["/store/bin/cargo", "/store/bin/targo"];

    /// Only a member no test binary started is its child's to judge: a deeper member stranded
    /// under a test binary (`deps/`) is that test's to bound, so its child, whose leader started,
    /// ends at its ceiling as the finding it was; the same member under cargo ends unrun as the
    /// stranded leader does, named by its own executable.
    #[cfg(unix)]
    #[test]
    fn a_member_under_a_test_binary_is_the_tests_to_bound() {
        let tmp = crate::mktemp_dir("atv-stranded-member").expect("mktemp");
        // Independent groups: the negative control's ceiling can elapse while the positive
        // control waits for its second program-check observation.
        let under_cargo = tmp.join("under-cargo.pid");
        let cargo_run = running(
            stand_in(&under_cargo, "exec sleep 600"),
            &tmp,
            DEFAULT_CHILD_CEILING,
            member_of(HeldAt::stranded(), &UNDER_CARGO),
        );
        let under_test = tmp.join("under-test.pid");
        let out = ended(
            running(
                stand_in(&under_test, "exec sleep 600"),
                &tmp,
                Duration::from_secs(9),
                member_of(HeldAt::stranded(), &["/r/target/debug/deps/band-1"]),
            ),
            &under_test,
            "a member under a test binary",
        );
        let cargo_out = ended(cargo_run, &under_cargo, "a member under cargo");
        assert!(
            out.spawn_error.is_none() && out.output.contains(crate::findings::CEILING_KILL),
            "{out:?}"
        );
        let out = cargo_out;
        let why = out.spawn_error.as_deref().expect("ended unrun");
        assert!(
            why.contains("build-script-build")
                && why.contains("(pid 999999)")
                && why.contains(&format!("(pid {})", HeldAt::DAEMON)),
            "{why}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// At its ceiling, a member under cargo that its watch has seen held for the bound on a first
    /// exec (11 min by the look the ceiling takes) decided nothing, though its leader started, so
    /// the child ends unrun, naming it, with no ceiling finding: its syspolicyd is older than its
    /// first exec, so it was never stranded. Negative controls: the same member seen held 1 min
    /// (a first exec within the bound is work in line), and one under a test binary, end at the
    /// ceiling as the finding they were.
    #[cfg(unix)]
    #[test]
    fn a_member_held_past_the_first_exec_bound_at_its_ceiling_ends_unrun() {
        let tmp = crate::mktemp_dir("atv-held-member").expect("mktemp");
        let older = || {
            HeldAt::leader(
                Duration::from_secs(60),
                Some(-40 * 60),
                Some(false),
                Some(0),
            )
        };
        let mut within = older();
        within.step = Duration::from_secs(60);
        let cases = [
            ("past the bound", member_of(older(), &UNDER_CARGO), true),
            ("within the bound", member_of(within, &UNDER_CARGO), false),
            (
                "under a test binary",
                member_of(older(), &["/r/target/debug/deps/band-1"]),
                false,
            ),
        ]
        .map(|(what, host, unrun)| {
            let pidfile = tmp.join(format!("{what}.pid"));
            let run = running(
                stand_in(&pidfile, "exec sleep 600"),
                &tmp,
                Duration::from_secs(3),
                host,
            );
            (what, host, unrun, pidfile, run)
        });
        for (what, host, unrun, pidfile, run) in cases {
            let out = ended(run, &pidfile, what);
            assert!(!out.ok && out.code.is_none(), "{what}: {out:?}");
            assert_eq!(host.asked(Asked::Backlog), 0, "{what}: never stranded");
            if !unrun {
                assert!(
                    out.spawn_error.is_none() && out.output.contains(crate::findings::CEILING_KILL),
                    "{what}: {out:?}"
                );
                continue;
            }
            let why = out.spawn_error.as_deref().expect("ended unrun");
            assert!(
                why.contains("build-script-build")
                    && why.contains("(pid 999999)")
                    && !why.contains("stranded"),
                "{what}: {why}"
            );
            assert!(
                !out.output.contains(crate::findings::CEILING_KILL),
                "{what}: {}",
                out.output
            );
            let mut r = crate::Report::new("held");
            r.decide_child(&out, "targo test --no-run");
            assert!(
                r.outcomes()
                    .all(|(o, _)| o == crate::Outcome::Fail(crate::Severity::CouldNotRun)),
                "{what}: {}",
                r.render()
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A stranded member under a leader that started ends its child as the stuck holder's end
    /// does before the ceiling: its group gets `SIGTERM` first, and the leader's handler runs
    /// within its `term_grace`, or [`STUCK_GRACE`] when it has none, before the `SIGKILL`.
    /// Negative control: the same leader read as the stranded program itself, which never ran and
    /// so has no handler, gets `SIGKILL` alone, its grace notwithstanding.
    #[cfg(unix)]
    #[test]
    fn a_started_leader_over_a_stranded_member_gets_its_term_grace_first() {
        let tmp = crate::mktemp_dir("atv-stranded-grace").expect("mktemp");
        let twenty = Duration::from_secs(20);
        let cases = [
            (
                "a member",
                member_of(HeldAt::stranded(), &UNDER_CARGO),
                Some(twenty),
                true,
            ),
            (
                "a member under a leader with no grace of its own",
                member_of(HeldAt::stranded(), &UNDER_CARGO),
                None,
                true,
            ),
            (
                "the leader",
                HeldAt::stranded().leaked(),
                Some(twenty),
                false,
            ),
        ]
        .map(|(what, host, grace, termed)| {
            let pidfile = tmp.join(format!("{what}.pid"));
            let marker = tmp.join(format!("{what}.term"));
            let cmd = stand_in(
                &pidfile,
                &format!(
                    "trap 'echo term > \"{}\"; exit 0' TERM; while :; do sleep 1; done",
                    marker.display()
                ),
            );
            let cmd = match grace {
                Some(g) => cmd.term_grace(g),
                None => cmd,
            };
            let run = running(cmd, &tmp, DEFAULT_CHILD_CEILING, host);
            (what, termed, pidfile, marker, run)
        });
        for (what, termed, pidfile, marker, run) in cases {
            let out = ended(run, &pidfile, what);
            let why = out.spawn_error.as_deref().expect("ended unrun");
            assert!(why.contains("stranded"), "{what}: {why}");
            assert_eq!(
                std::fs::read_to_string(&marker).ok().as_deref(),
                termed.then_some("term\n"),
                "{what}: {why}"
            );
            let pid = written_pid(&pidfile).expect("its pid");
            assert!(gone_within(&pid, HANG_DETECTOR), "{what}: pid {pid}");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The scope of the stranded rule: a member's chain judged ([`start::in_scope`]) is read
    /// whole and holds no executable run from a `deps` directory, at any depth; the walk up the
    /// group ([`start::chain_of`]) reads each parent to the leader, and gives none when a record
    /// or a path is unread or a parent is outside the group.
    #[test]
    fn only_a_member_no_test_binary_started_is_its_childs_to_judge() {
        let p = |s: &str| PathBuf::from(s);
        let cargo = [p("/store/bin/cargo"), p("/store/bin/targo")];
        assert!(start::in_scope(Some(&[])));
        assert!(start::in_scope(Some(&cargo)));
        assert!(start::in_scope(Some(&[p("/deps/bin/cargo")])));
        assert!(!start::in_scope(Some(&[p("/r/target/debug/deps/band-1")])));
        assert!(!start::in_scope(Some(&[
            p("/bin/sh"),
            p("/r/target/debug/deps/band-1"),
            p("/store/bin/cargo"),
        ])));
        assert!(!start::in_scope(None));
        // pid: (parent, group); 10 leads group 10, 30 is in group 30.
        let record = |pid| match pid {
            10 => Some((1, 10)),
            11 => Some((10, 10)),
            12 => Some((11, 10)),
            13 => Some((30, 10)),
            14 => Some((15, 10)),
            30 => Some((1, 30)),
            _ => None,
        };
        let path = |pid| match pid {
            10 => Some(p("/store/bin/targo")),
            11 => Some(p("/store/bin/cargo")),
            _ => None,
        };
        let chain = |pid| start::chain_of(pid, 10, record, path);
        assert_eq!(chain(10), Some(Vec::new()));
        assert_eq!(
            chain(12),
            Some(vec![p("/store/bin/cargo"), p("/store/bin/targo")])
        );
        assert_eq!(chain(13), None, "a parent outside the group");
        assert_eq!(chain(14), None, "an unread parent");
        let unread_path = |pid| (pid != 11).then(|| p("/x"));
        assert_eq!(start::chain_of(12, 10, record, unread_path), None);
    }

    /// The live host reads this test process as started, and the syspolicyd running now as one
    /// with a pid, or none.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_live_host_reads_this_process_started_and_a_daemon_or_none() {
        let me = std::process::id();
        let pgid = aterm_sysprobe::kproc::record(me)
            .and_then(|r| u32::try_from(r.pgid).ok())
            .expect("this process's record");
        assert_eq!(
            LIVE_HOST.unstarted(pgid, &[me], aterm_sysprobe::jobs::now_us()),
            Vec::new()
        );
        match LIVE_HOST.daemon() {
            DaemonNow::Running(d) => assert!(d.pid > 1, "{d:?}"),
            DaemonNow::NoneRunning => {}
            DaemonNow::Unread => panic!("the process table reads"),
        }
    }

    /// CEILING-WAIT's wait for a core: a stand-in child whose tree is given 5% of the time it is
    /// runnable, under a 3 s ceiling, still runs 7 s in, whether its host books that time as it
    /// passes or in lumps a second apart, twice its reads' window. Given every core it asks for,
    /// the ceiling ends it at 3 s of work, and the kill line names apart the time it waited for a
    /// core. Negative controls: the same child on a host with no reading of its tree (the clock
    /// before this step) is ended at its ceiling while it waits, naming no core; a clock that
    /// judges each read alone ends the lumpy one while it is starved.
    #[cfg(unix)]
    #[test]
    fn a_child_starved_of_cores_is_not_ended_for_the_time_it_waited() {
        let tmp = crate::mktemp_dir("atv-starved").expect("mktemp");
        let ceiling = Duration::from_secs(3);
        let control = tmp.join("control.pid");
        let control_run = running(stand_in(&control, "exec sleep 600"), &tmp, ceiling, &INERT);
        let starved = [
            ("as it passes", Starving::at(50)),
            ("in lumps", Starving::lumpy(50, Duration::from_secs(1))),
        ]
        .map(|(how, host)| {
            let pidfile = tmp.join(format!("starved {how}.pid"));
            let run = running(stand_in(&pidfile, "exec sleep 600"), &tmp, ceiling, host);
            (how, host, pidfile, run)
        });
        // Hold them starved past the ceiling: the condition under test, not a wait for anything.
        std::thread::sleep(ceiling + Duration::from_secs(4));
        let held = starved
            .each_ref()
            .map(|(_, _, _, run)| !run.0.is_finished());
        for (_, host, _, _) in &starved {
            host.give(1000);
        }
        let control_out = ended(control_run, &control, "the control");
        for ((how, _, pidfile, run), held) in starved.into_iter().zip(held) {
            let out = ended(run, &pidfile, &format!("booked {how}, given every core"));
            assert!(
                held,
                "the ceiling ended a child starved {how}: {}",
                out.output
            );
            assert!(!out.ok && out.code.is_none(), "{how}: {out:?}");
            assert!(
                out.usage.is_some_and(|u| u.starved > Duration::ZERO),
                "{how}: its usage names the wait for a core: {:?}",
                out.usage
            );
            assert!(
                out.output.contains(crate::findings::CEILING_KILL)
                    && out.output.contains("of work, over the 3.0s ceiling")
                    && out
                        .output
                        .contains("for a core, which the ceiling leaves out")
                    && !out.output.contains("admission or a lock"),
                "{how}: {}",
                out.output
            );
        }
        let out = control_out;
        assert!(
            out.output.contains("of work, over the 3.0s ceiling")
                && !out.output.contains("for a core")
                && out.usage.is_none(),
            "{}",
            out.output
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A tree runnable all the time but given no core at all stays idle: after
    /// `IDLE_HANG` its time counts as work again, so a 1 s ceiling ends it, naming the time
    /// it waited for a core before that apart. Negative control: with no such bound the ceiling
    /// never ends it, and the hang detector fails the test.
    #[cfg(unix)]
    #[test]
    fn a_tree_given_no_core_at_all_is_a_hang_its_ceiling_ends() {
        let tmp = crate::mktemp_dir("atv-no-core").expect("mktemp");
        let pidfile = tmp.join("no-core.pid");
        let out = ended(
            running(
                stand_in(&pidfile, "exec sleep 600"),
                &tmp,
                Duration::from_secs(1),
                Starving::at(0),
            ),
            &pidfile,
            "a tree given no core",
        );
        assert!(
            out.output.contains("of work, over the 1.0s ceiling")
                && out
                    .output
                    .contains("for a core, which the ceiling leaves out"),
            "{}",
            out.output
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// On the OS's reading of its tree, a stopped child is runnable for none of its time, so its
    /// ceiling ends it as a hang; and a busy-looping child, whatever share of the cores it gets,
    /// still ends at its ceiling of work. Negative controls: a clock that counts all of a tree's
    /// runnable time as waiting never ends the busy one; one that counts a window its tree was
    /// runnable for none of as a wait ends the stopped one only at `IDLE_HANG`, and with
    /// that bound gone too never ends it (the arithmetic test fails on the first alone); the hang
    /// detector fails each that never ends.
    #[cfg(unix)]
    #[test]
    fn a_stopped_child_and_a_busy_one_still_end_at_their_ceiling() {
        let tmp = crate::mktemp_dir("atv-stopped-busy").expect("mktemp");
        for (name, then) in [
            ("stopped", "kill -STOP $$"),
            ("busy", "while :; do :; done"),
        ] {
            let pidfile = tmp.join(format!("{name}.pid"));
            let out = ended(
                running(
                    stand_in(&pidfile, then),
                    &tmp,
                    Duration::from_secs(1),
                    &LIVE_HOST,
                ),
                &pidfile,
                name,
            );
            assert!(
                !out.ok
                    && out.output.contains(crate::findings::CEILING_KILL)
                    && out.output.contains("of work, over the 1.0s ceiling"),
                "{name}: {}",
                out.output
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The arithmetic of a window: the part of a window a tree that used `used` in it waited for a
    /// core. Negative controls are the cases each rule decides.
    #[test]
    fn a_window_waits_for_a_core_only_while_its_tree_is_runnable_and_not_running() {
        let ms = Duration::from_millis;
        let used = |cpu: u64, runnable: u64| Used {
            cpu: ms(cpu),
            runnable: ms(runnable),
            io: 0,
        };
        let w = ms(1000);
        let waits = |used: Used, want: Duration| {
            let got = core_wait(w, used, 18);
            assert!(
                got.abs_diff(want) < Duration::from_micros(1),
                "{used:?}: {got:?}, not {want:?}"
            );
        };
        // One thread runnable the whole window, on a core 30% of it: 700 ms waited.
        waits(used(300, 1000), ms(700));
        // Forty threads on 18 cores alone, getting nearly all of them: its own crowd, 50 ms.
        waits(used(17_100, 40_000), ms(50));
        // Ten threads given 10%: 900 ms of the window, not the 9 s their waits sum to.
        waits(used(1000, 10_000), ms(900));
        // One thread runnable half the window, on a core half of that: 250 ms, never 500.
        waits(used(250, 500), ms(250));
        // A blocked tree whose timer thread waited 40 µs to run: 40 µs, never the window.
        waits(
            Used {
                cpu: Duration::from_micros(10),
                runnable: Duration::from_micros(50),
                io: 0,
            },
            Duration::from_micros(40),
        );
        // Stopped, or blocked: runnable for none of it.
        waits(used(0, 0), Duration::ZERO);
        // Running all it was runnable: no wait.
        waits(used(1000, 1000), Duration::ZERO);
    }

    /// A test binary is hung only after both progress generations stay fixed for the complete
    /// evidence window. A complete line or any tree CPU restarts it; an unread tree disarms it.
    /// NEGATIVE CONTROLS: wall time with unchanged generations convicts at exactly the window,
    /// while each changed generation and `None` do not.
    #[test]
    fn test_progress_requires_no_complete_line_and_no_tree_cpu() {
        let window = Duration::from_secs(180);
        let begun = Instant::now();

        let mut blocked = TestProgress::new(window, begun);
        assert!(
            !blocked.hung(begun, Some(7), Some(11)),
            "the baseline only arms"
        );
        assert!(!blocked.hung(begun + window - Duration::from_nanos(1), Some(7), Some(11)));
        assert!(blocked.hung(begun + window, Some(7), Some(11)));

        let mut lines = TestProgress::new(window, begun);
        assert!(!lines.hung(begun, Some(1), Some(0)));
        assert!(!lines.hung(begun + window, Some(2), Some(0)));
        assert!(!lines.hung(
            begun + window * 2 - Duration::from_nanos(1),
            Some(2),
            Some(0)
        ));
        assert!(lines.hung(begun + window * 2, Some(2), Some(0)));

        let mut cpu = TestProgress::new(window, begun);
        assert!(!cpu.hung(begun, Some(1), Some(4)));
        assert!(!cpu.hung(begun + window, Some(1), Some(5)));
        assert!(!cpu.hung(
            begun + window * 2 - Duration::from_nanos(1),
            Some(1),
            Some(5)
        ));
        assert!(cpu.hung(begun + window * 2, Some(1), Some(5)));

        let mut unread = TestProgress::new(window, begun);
        assert!(!unread.hung(begun, Some(1), Some(0)));
        assert!(!unread.hung(begun + window, Some(1), None));
        assert!(!unread.hung(begun + window * 2, Some(1), Some(0)));
        assert!(unread.hung(begun + window * 3, Some(1), Some(0)));
        assert!(!unread.hung(begun + window * 4, None, Some(0)));
    }

    /// The complete-tree probe follows parentage, not process groups, and carries reaped CPU in
    /// the owning row. An incomplete table or unread counter proves nothing. NEGATIVE CONTROL:
    /// the child and grandchild use different process groups from the root but remain present.
    #[test]
    fn tree_cpu_includes_descendants_in_other_groups_and_fails_open() {
        let process = |pid, ppid, pgid, start, cpu, reaped| aterm_sysprobe::ProcessReading {
            pid,
            ppid,
            pgid,
            start: Some(start),
            cpu_ns: Some(cpu),
            child_cpu_ns: Some(reaped),
            ..aterm_sysprobe::ProcessReading::default()
        };
        let mut sweep = aterm_sysprobe::Sweep {
            procs: vec![
                process(40, 1, 40, 400, 10, 5),
                process(41, 40, 41, 410, 20, 0),
                process(42, 41, 99, 420, 30, 7),
                process(70, 1, 70, 700, 1_000, 0),
            ],
            truncated: false,
        };
        assert_eq!(
            tree_usage_from_sweep(&sweep, 40),
            Some(vec![
                TreeMember {
                    pid: 40,
                    start: 400,
                    cpu_ns: 15,
                },
                TreeMember {
                    pid: 41,
                    start: 410,
                    cpu_ns: 20,
                },
                TreeMember {
                    pid: 42,
                    start: 420,
                    cpu_ns: 37,
                },
            ])
        );
        sweep.procs[0].child_cpu_ns = None;
        assert_eq!(
            tree_usage_from_sweep(&sweep, 40),
            None,
            "an unread cumulative reaped-child counter fails open; short-lived busy children may have escaped the sweep"
        );
        sweep.procs[0].child_cpu_ns = Some(5);
        sweep.truncated = true;
        assert_eq!(tree_usage_from_sweep(&sweep, 40), None);
        sweep.truncated = false;
        sweep.procs[1].cpu_ns = None;
        assert_eq!(tree_usage_from_sweep(&sweep, 40), None);
    }

    #[cfg(unix)]
    const HUNG_CASE_FIXTURE: &str = "ATERM_VERIFY_HUNG_CASE_FIXTURE";
    #[cfg(unix)]
    const HUNG_CASE_SOCKET: &str = "ATERM_VERIFY_HUNG_CASE_SOCKET";
    #[cfg(unix)]
    const HUNG_BUSY_WORKER_SOCKET: &str = "ATERM_VERIFY_HUNG_BUSY_WORKER_SOCKET";

    /// A real libtest case used by the no-progress controls. Each mode becomes ready by binding
    /// its datagram before it stops printing. `blocked` and `resume` wait in the kernel;
    /// `busy-parent` consumes CPU until its probe releases it; and `busy-child` waits on another
    /// copy of this test binary which consumes the CPU. There is no time-based fixture behavior.
    #[cfg(unix)]
    #[test]
    fn hung_case_fixture() {
        let Some(mode) = std::env::var_os(HUNG_CASE_FIXTURE) else {
            return;
        };
        let path = PathBuf::from(std::env::var_os(HUNG_CASE_SOCKET).expect("fixture socket"));
        match mode.to_str().expect("fixture mode") {
            "blocked" | "resume" => {
                let socket = std::os::unix::net::UnixDatagram::bind(path).expect("bind fixture");
                let mut byte = [0_u8];
                socket.recv(&mut byte).expect("release fixture");
            }
            "busy-parent" => busy_until_datagram(&path),
            "busy-child" => {
                let mut child =
                    aterm_uds::spawnfd::command(std::env::current_exe().expect("this test binary"));
                child
                    .args([
                        "--exact",
                        "exec::tests::hung_busy_worker_fixture",
                        "--test-threads=1",
                    ])
                    .env(HUNG_BUSY_WORKER_SOCKET, &path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let mut child = child.spawn().expect("spawn busy descendant");
                assert!(child.wait().expect("wait for busy descendant").success());
            }
            other => panic!("unknown hung fixture mode {other}"),
        }
    }

    /// The actual busy descendant of [`hung_case_fixture`]. Its output is sent to `/dev/null`,
    /// so only its CPU can keep the parent binary alive.
    #[cfg(unix)]
    #[test]
    fn hung_busy_worker_fixture() {
        let Some(path) = std::env::var_os(HUNG_BUSY_WORKER_SOCKET) else {
            return;
        };
        busy_until_datagram(Path::new(&path));
    }

    #[cfg(unix)]
    fn busy_until_datagram(path: &Path) {
        let socket = std::os::unix::net::UnixDatagram::bind(path).expect("bind busy fixture");
        socket.set_nonblocking(true).expect("nonblocking fixture");
        let mut byte = [0_u8];
        loop {
            match socket.recv(&mut byte) {
                Ok(_) => return,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::hint::black_box(byte[0]);
                }
                Err(error) => panic!("receive fixture release: {error}"),
            }
        }
    }

    #[cfg(unix)]
    #[derive(Clone, Copy, Debug)]
    enum ProgressKind {
        Flat,
        Leader,
        Descendant,
        ResumeAfterSample,
        UnreadCacheAfterSample,
        SampleError,
    }

    /// An injected complete descendant-tree probe. Its counter is the deterministic clock for
    /// the live controls; `sampled_alive` proves sampling happened before the local kill.
    #[cfg(unix)]
    #[derive(Debug)]
    struct ProgressHost {
        kind: ProgressKind,
        reads: AtomicU64,
        root: std::sync::atomic::AtomicU32,
        sampled_alive: std::sync::atomic::AtomicBool,
        sample_calls: AtomicU64,
        fresh_reads: AtomicU64,
        post_sample_cpu: std::sync::atomic::AtomicBool,
        released: std::sync::atomic::AtomicBool,
        socket: PathBuf,
    }

    #[cfg(unix)]
    impl ProgressHost {
        fn new(kind: ProgressKind, socket: PathBuf) -> Self {
            Self {
                kind,
                reads: AtomicU64::new(0),
                root: std::sync::atomic::AtomicU32::new(0),
                sampled_alive: std::sync::atomic::AtomicBool::new(false),
                sample_calls: AtomicU64::new(0),
                fresh_reads: AtomicU64::new(0),
                post_sample_cpu: std::sync::atomic::AtomicBool::new(false),
                released: std::sync::atomic::AtomicBool::new(false),
                socket,
            }
        }

        fn release(&self) {
            if self.released.swap(true, Ordering::SeqCst) {
                return;
            }
            std::os::unix::net::UnixDatagram::unbound()
                .and_then(|socket| socket.send_to(&[1], &self.socket))
                .expect("release live hang fixture");
        }

        fn reading(&self, root: u32, fresh: bool) -> Option<Vec<TreeMember>> {
            self.root.store(root, Ordering::SeqCst);
            if !self.socket.exists() {
                return None;
            }
            let post_sample_cpu = self.post_sample_cpu.load(Ordering::SeqCst);
            if matches!(self.kind, ProgressKind::UnreadCacheAfterSample)
                && post_sample_cpu
                && !fresh
            {
                return None;
            }
            let tick = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            let member = |pid, start, cpu_ns| TreeMember { pid, start, cpu_ns };
            let reading = match self.kind {
                ProgressKind::Flat | ProgressKind::SampleError => vec![member(root, 1, 0)],
                ProgressKind::ResumeAfterSample | ProgressKind::UnreadCacheAfterSample => {
                    vec![member(root, 1, u64::from(fresh && post_sample_cpu))]
                }
                ProgressKind::Leader => vec![member(root, 1, tick)],
                ProgressKind::Descendant => vec![
                    member(root, 1, 0),
                    member(root.saturating_add(10_000), 2, tick),
                ],
            };
            if matches!(self.kind, ProgressKind::Leader | ProgressKind::Descendant) && tick >= 4 {
                self.release();
            }
            Some(reading)
        }
    }

    #[cfg(unix)]
    impl Host for ProgressHost {
        fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
            self.root.store(pgid, Ordering::SeqCst);
            Some(vec![aterm_sysprobe::MemberUsage {
                pid: pgid,
                start: 1,
                cpu_ns: self.reads.load(Ordering::SeqCst),
                ..aterm_sysprobe::MemberUsage::default()
            }])
        }

        fn tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
            self.reading(root, false)
        }

        fn fresh_tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
            self.fresh_reads.fetch_add(1, Ordering::SeqCst);
            let reading = self.reading(root, true);
            if matches!(
                self.kind,
                ProgressKind::ResumeAfterSample | ProgressKind::UnreadCacheAfterSample
            ) && self.post_sample_cpu.load(Ordering::SeqCst)
            {
                self.release();
            }
            reading
        }

        fn usage_every(&self) -> Duration {
            Duration::ZERO
        }

        fn sample(&self, pid: u32, output_dir: &Path, canceled: &dyn Fn() -> bool) -> StackSample {
            if canceled() {
                return StackSample {
                    report: None,
                    error: Some("sample canceled".to_owned()),
                };
            }
            self.sample_calls.fetch_add(1, Ordering::SeqCst);
            let alive = i32::try_from(pid)
                .ok()
                .and_then(aterm_uds::process::read_identity)
                .is_some();
            self.sampled_alive.store(alive, Ordering::SeqCst);
            let report = output_dir.join(format!("hung-{pid}.sample.txt"));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
            if let Err(error) = options
                .open(&report)
                .and_then(|mut file| file.write_all(b"injected all-thread sample\n"))
            {
                return StackSample {
                    report: None,
                    error: Some(error.to_string()),
                };
            }
            if matches!(
                self.kind,
                ProgressKind::ResumeAfterSample | ProgressKind::UnreadCacheAfterSample
            ) {
                // Ordinary reads deliberately retain the cached zero-CPU view. Only the
                // post-sample fresh read observes this progress, then releases the fixture.
                self.post_sample_cpu.store(true, Ordering::SeqCst);
            }
            StackSample {
                report: Some(report),
                error: matches!(
                    self.kind,
                    ProgressKind::ResumeAfterSample
                        | ProgressKind::UnreadCacheAfterSample
                        | ProgressKind::SampleError
                )
                .then(|| "injected sampler failure after writing the report".to_owned()),
            }
        }

        fn idle(&self) -> Option<Duration> {
            None
        }

        fn cores(&self) -> u32 {
            1
        }
    }

    #[cfg(unix)]
    fn hung_fixture_command(
        mode: &str,
        socket: &Path,
        hang_after: Duration,
        stop_at_red: bool,
    ) -> Cmd {
        let binary = std::env::current_exe().expect("this test binary");
        let mut command = Cmd::new(&binary)
            .args([
                "-Z",
                "unstable-options",
                "--format",
                "json",
                "--exact",
                "exec::tests::hung_case_fixture",
                "--test-threads=1",
            ])
            .env(HUNG_CASE_FIXTURE, mode)
            .env(HUNG_CASE_SOCKET, socket);
        command.test_cases = Some(CaseWatch {
            spec: "-p aterm-verify --lib".into(),
            binary: binary.display().to_string(),
            json: true,
            stop_at_red,
            hang_after,
        });
        command
    }

    /// Run a live fixture under an outer one-minute hang detector. That minute is only the
    /// test's failure detector: every successful injected decision is immediate. On failure the
    /// still-unreaped leader retains its group id, so cleanup can signal exactly that group.
    #[cfg(unix)]
    fn run_hung_fixture(
        cmd: Cmd,
        dir: &Path,
        host: std::sync::Arc<ProgressHost>,
        red_alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
        red_calls: std::sync::Arc<AtomicU64>,
        fail_case_open: bool,
    ) -> Run {
        let dir = dir.to_path_buf();
        let (sent, answer) = std::sync::mpsc::channel();
        let thread_host = std::sync::Arc::clone(&host);
        let thread = std::thread::spawn(move || {
            let samples = dir.join("hung-samples");
            let env = ExecEnv {
                host: thread_host.as_ref(),
                hang_samples: Some(&samples),
                ..ceiled(&dir, None)
            };
            let callback_host = std::sync::Arc::clone(&thread_host);
            let callback = || {
                red_calls.fetch_add(1, Ordering::SeqCst);
                let pid = callback_host.root.load(Ordering::SeqCst);
                let alive = i32::try_from(pid)
                    .ok()
                    .and_then(aterm_uds::process::read_identity)
                    .is_some();
                red_alive.store(alive, Ordering::SeqCst);
            };
            let run = if fail_case_open {
                run_test_opening(&cmd, env, &callback, &|_, _| {
                    Err(io::Error::other("injected live-capture open failure"))
                })
            } else {
                run_test(&cmd, env, &callback)
            };
            let _ = sent.send(run);
        });
        match answer.recv_timeout(HANG_DETECTOR) {
            Ok(run) => {
                let _ = thread.join();
                run
            }
            Err(error) => {
                let pid = host.root.load(Ordering::SeqCst);
                if let Ok(pid) = i32::try_from(pid)
                    && pid > 1
                {
                    // SAFETY: the worker retains this exact leader unreaped, so `-pid` still
                    // names its owned process group. This is only the outer detector's failure
                    // cleanup.
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
                // Never replace the outer detector with an unbounded join. A killed fixture
                // normally unwinds its worker promptly; if verify itself is wedged, detach it
                // and let this test fail now.
                if answer.recv_timeout(Duration::from_secs(5)).is_ok() {
                    let _ = thread.join();
                }
                panic!("the live hang control exceeded its one-minute detector: {error}");
            }
        }
    }

    /// A blocked real libtest binary is sampled, named with its unfinished case, made the local
    /// first red, and killed. NEGATIVE CONTROL: both the sampler and the local halt callback
    /// observe its identity alive, proving they precede the kill.
    #[cfg(unix)]
    #[test]
    fn a_blocked_test_binary_is_sampled_and_ends_as_hung() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = aterm_tempfile::tempdir().expect("fixture root");
        let socket = root.path().join("fixture.sock");
        let samples = root.path().join("hung-samples");
        let host = std::sync::Arc::new(ProgressHost::new(ProgressKind::Flat, socket.clone()));
        let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let red_calls = std::sync::Arc::new(AtomicU64::new(0));
        let run = run_hung_fixture(
            hung_fixture_command("blocked", &socket, Duration::ZERO, true),
            root.path(),
            std::sync::Arc::clone(&host),
            std::sync::Arc::clone(&red_alive),
            std::sync::Arc::clone(&red_calls),
            false,
        );
        let hung = run.hung.as_ref().expect("a named hung red");
        assert!(!run.ok && run.spawn_error.is_none(), "{run:?}");
        assert_eq!(hung.spec, "-p aterm-verify --lib");
        assert_eq!(
            hung.binary,
            std::env::current_exe().unwrap().display().to_string()
        );
        assert_eq!(hung.window, Duration::ZERO);
        assert_eq!(hung.running, ["exec::tests::hung_case_fixture"]);
        assert!(run.output.contains("HUNG") && run.output.contains("hung_case_fixture"));
        assert!(run.environment_failure().is_none(), "a hang is a tree red");
        let sample = hung.sample.report.as_ref().expect("retained sample");
        assert!(hung.sample.error.is_none(), "{:?}", hung.sample);
        assert!(
            sample.starts_with(&samples) && sample.is_file(),
            "{sample:?}"
        );
        assert_eq!(
            std::fs::metadata(sample).unwrap().permissions().mode() & 0o077,
            0,
            "the raw stack is owner-private"
        );
        assert!(host.sampled_alive.load(Ordering::SeqCst));
        assert_eq!(host.sample_calls.load(Ordering::SeqCst), 1);
        assert!(red_alive.load(Ordering::SeqCst));
        assert_eq!(red_calls.load(Ordering::SeqCst), 1);
    }

    /// A spawned binary whose live capture cannot be opened is ended immediately as NOT RUN;
    /// monitoring is never silently disabled. NEGATIVE CONTROL: the real fixture blocks forever,
    /// so converting the injected error to `None` reaches the outer one-minute hang detector.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_live_capture_ends_the_test_binary_unrun() {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        let socket = root.path().join("fixture.sock");
        let host = std::sync::Arc::new(ProgressHost::new(ProgressKind::Flat, socket.clone()));
        let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let red_calls = std::sync::Arc::new(AtomicU64::new(0));
        let run = run_hung_fixture(
            hung_fixture_command("blocked", &socket, Duration::ZERO, true),
            root.path(),
            std::sync::Arc::clone(&host),
            std::sync::Arc::clone(&red_alive),
            std::sync::Arc::clone(&red_calls),
            true,
        );
        assert!(!run.ok && run.hung.is_none(), "{run:?}");
        assert!(
            run.spawn_error
                .as_deref()
                .is_some_and(|why| why.contains("cannot monitor")
                    && why.contains("injected live-capture open failure")),
            "{run:?}"
        );
        assert!(run.output.contains("NOT RUN"), "{}", run.output);
        assert_eq!(host.sample_calls.load(Ordering::SeqCst), 0);
        assert_eq!(red_calls.load(Ordering::SeqCst), 0);
        assert!(!red_alive.load(Ordering::SeqCst));
    }

    /// CPU by the binary itself, and CPU by a descendant while the binary waits, both keep it
    /// live. The descendant control keeps the root counter flat. NEGATIVE CONTROL: if either
    /// progress generation were ignored, the zero evidence window would sample and kill it.
    #[cfg(unix)]
    #[test]
    fn cpu_by_a_test_binary_or_its_busy_child_prevents_a_hung_red() {
        for (mode, kind) in [
            ("busy-parent", ProgressKind::Leader),
            ("busy-child", ProgressKind::Descendant),
        ] {
            let root = aterm_tempfile::tempdir().expect("fixture root");
            let socket = root.path().join("fixture.sock");
            let host = std::sync::Arc::new(ProgressHost::new(kind, socket.clone()));
            let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let red_calls = std::sync::Arc::new(AtomicU64::new(0));
            let run = run_hung_fixture(
                hung_fixture_command(mode, &socket, Duration::ZERO, false),
                root.path(),
                std::sync::Arc::clone(&host),
                std::sync::Arc::clone(&red_alive),
                std::sync::Arc::clone(&red_calls),
                false,
            );
            assert!(run.ok && run.hung.is_none(), "{mode}: {run:?}");
            assert_eq!(host.sample_calls.load(Ordering::SeqCst), 0, "{mode}");
            assert_eq!(red_calls.load(Ordering::SeqCst), 0, "{mode}");
            assert!(!red_alive.load(Ordering::SeqCst), "{mode}");
        }
    }

    /// A tentative no-progress finding is not a verdict: sampling makes CPU progress available
    /// while ordinary reads deliberately retain either a cached zero-CPU view or an unread view.
    /// Only the independent post-sample sweep observes progress and releases the blocked binary
    /// before any kill or red. NEGATIVE CONTROLS: using the cadence cache for confirmation makes
    /// the first binary HUNG; letting an unread cached view erase the pre-sample baseline makes
    /// the second HUNG. The discarded samples are removed, while each shared run-owned directory
    /// stays available to another binary that may already be preparing its own sample.
    #[cfg(unix)]
    #[test]
    fn progress_during_the_stack_sample_cancels_the_hung_finding() {
        for (cache, kind) in [
            ("stale zero", ProgressKind::ResumeAfterSample),
            ("unread", ProgressKind::UnreadCacheAfterSample),
        ] {
            let root = aterm_tempfile::tempdir().expect("fixture root");
            let socket = root.path().join("fixture.sock");
            let samples = root.path().join("hung-samples");
            let host = std::sync::Arc::new(ProgressHost::new(kind, socket.clone()));
            let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let red_calls = std::sync::Arc::new(AtomicU64::new(0));
            let run = run_hung_fixture(
                hung_fixture_command("resume", &socket, Duration::ZERO, true),
                root.path(),
                std::sync::Arc::clone(&host),
                std::sync::Arc::clone(&red_alive),
                std::sync::Arc::clone(&red_calls),
                false,
            );
            assert!(run.ok && run.hung.is_none(), "{cache}: {run:?}");
            assert_eq!(host.sample_calls.load(Ordering::SeqCst), 1, "{cache}");
            assert_eq!(
                host.fresh_reads.load(Ordering::SeqCst),
                1,
                "{cache}: confirmation must bypass the shared process-table cache exactly once"
            );
            assert!(
                samples.is_dir(),
                "{cache}: the shared sample directory remains available"
            );
            assert_eq!(
                std::fs::read_dir(&samples)
                    .expect("retained sample directory")
                    .count(),
                0,
                "{cache}: only this canceled report was discarded"
            );
            assert_eq!(red_calls.load(Ordering::SeqCst), 0, "{cache}");
            assert!(!red_alive.load(Ordering::SeqCst), "{cache}");
        }
    }

    /// A native sampler can fail after reserving and partly writing its private report. That
    /// diagnostic belongs to the hung red rather than being orphaned. NEGATIVE CONTROL: the
    /// injected sampler returns an error as well as the report; either half being discarded makes
    /// the assertions fail.
    #[cfg(unix)]
    #[test]
    fn a_failed_sampler_retains_and_names_its_partial_report() {
        let root = aterm_tempfile::tempdir().expect("fixture root");
        let socket = root.path().join("fixture.sock");
        let samples = root.path().join("hung-samples");
        let host =
            std::sync::Arc::new(ProgressHost::new(ProgressKind::SampleError, socket.clone()));
        let red_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let red_calls = std::sync::Arc::new(AtomicU64::new(0));
        let run = run_hung_fixture(
            hung_fixture_command("blocked", &socket, Duration::ZERO, true),
            root.path(),
            std::sync::Arc::clone(&host),
            std::sync::Arc::clone(&red_alive),
            std::sync::Arc::clone(&red_calls),
            false,
        );
        let hung = run.hung.as_ref().expect("a named hung red");
        let report = hung
            .sample
            .report
            .as_ref()
            .expect("the partial sample report");
        assert!(
            report.starts_with(&samples) && report.is_file(),
            "{report:?}"
        );
        assert_eq!(
            hung.sample.error.as_deref(),
            Some("injected sampler failure after writing the report")
        );
        assert!(
            run.output.contains("partial stack sample")
                && run.output.contains("collection failed")
                && run.output.contains(&report.display().to_string()),
            "{}",
            run.output
        );
        assert_eq!(host.sample_calls.load(Ordering::SeqCst), 1);
        assert!(host.sampled_alive.load(Ordering::SeqCst));
        assert_eq!(red_calls.load(Ordering::SeqCst), 1);
        assert!(red_alive.load(Ordering::SeqCst));
    }

    /// The arithmetic of a span of reads ([`CoreWaitSpan::book`]), 500 ms apart on 18 cores.
    /// - One thread given 5% whose time the kernel books every fourth read, in a lump: each lump
    ///   moves all 1.9 s it waited, and the reads between move nothing. Negative control: judged
    ///   read by read, the same lumps move 0.475 s each, a quarter of the wait.
    /// - Ten seconds of 18 threads on every core, then ten of one thread given 5%: by the end of
    ///   the second ten the span has moved all 9.5 s of its wait. Negative control: judged on the
    ///   totals since the first read, the busy past leaves 1 s.
    /// - Four threads given 5% that then stop: the reads with nothing booked move nothing, though
    ///   the span's share alone would call them starved; a booking after them moves their wait.
    /// - What the span's reads moved never passes the work counted in them, nor the clock's work;
    ///   and once forgotten (idle for `IDLE_HANG`), a lump moves at most its own
    ///   read's work.
    #[test]
    fn a_span_of_reads_moves_a_wait_the_kernel_books_late() {
        let ms = Duration::from_millis;
        let close = |got: Duration, want: Duration, what: &str| {
            assert!(
                got.abs_diff(want) < Duration::from_micros(10),
                "{what}: {got:?}, not {want:?}"
            );
        };
        let used = |cpu: u64, runnable: u64| Used {
            cpu: ms(cpu),
            runnable: ms(runnable),
            io: 0,
        };
        let w = ms(500);
        let lots = Duration::from_secs(3600);
        let lump = |i: usize| {
            if i % 4 == 3 {
                used(100, 2000)
            } else {
                used(0, 0)
            }
        };

        let mut span = CoreWaitSpan::default();
        let mut total = Duration::ZERO;
        for i in 0..40 {
            let moved = span.book(w, lump(i), w, 18, lots);
            let want = if i % 4 == 3 { ms(1900) } else { Duration::ZERO };
            close(moved, want, &format!("read {i}"));
            total += moved;
        }
        close(total, ms(19_000), "ten lumps");
        let read_by_read: Duration = (0..40).map(|i| core_wait(w, lump(i), 18)).sum();
        close(read_by_read, ms(4750), "the control, read by read");

        let mut span = CoreWaitSpan::default();
        let (busy, starved) = (used(9000, 9000), used(25, 500));
        let mut total = Duration::ZERO;
        for i in 0..40 {
            total += span.book(w, if i < 20 { busy } else { starved }, w, 18, lots);
        }
        close(total, ms(9500), "the starved ten seconds");
        let all = Used {
            cpu: ms(20 * 9000 + 20 * 25),
            runnable: ms(20 * 9000 + 20 * 500),
            io: 0,
        };
        close(
            core_wait(Duration::from_secs(20), all, 18),
            ms(1000),
            "the control, on the totals",
        );

        let mut span = CoreWaitSpan::default();
        for _ in 0..20 {
            span.book(w, used(100, 2000), w, 18, lots);
        }
        for i in 0..5 {
            let moved = span.book(w, used(0, 0), w, 18, lots);
            close(
                moved,
                Duration::ZERO,
                &format!("four threads stopped, read {i}"),
            );
        }
        close(
            span.book(w, used(100, 2000), w, 18, lots),
            ms(6 * 475),
            "four threads given 5% again: a booking covers the reads before it",
        );

        let mut span = CoreWaitSpan::default();
        let moved: Duration = (0..4)
            .map(|i| span.book(w, lump(i), ms(100), 18, lots))
            .sum();
        close(moved, ms(400), "at most the span's work");
        let mut span = CoreWaitSpan::default();
        let moved: Duration = (0..4).map(|i| span.book(w, lump(i), w, 18, ms(200))).sum();
        close(moved, ms(200), "at most the clock's work");
        let mut span = CoreWaitSpan::default();
        for _ in 0..20 {
            span.book(w, starved, w, 18, lots);
        }
        span.clear();
        let moved = span.book(w, used(100, 12_000), w, 18, lots);
        assert!(
            moved <= w && !moved.is_zero(),
            "a lump after a hang: {moved:?}"
        );
    }

    /// The kill line names a wait the ceiling left out only once it shows as `0.1s` or more, by its
    /// own figure; it tells a wait for admission or a lock from one for a core, and names both
    /// together.
    #[test]
    fn a_ceiling_kill_names_the_waits_it_left_out() {
        let s = Duration::from_secs_f64;
        assert_eq!(waited_clause(s(0.049), s(0.049)), "", "neither shows");
        assert!(
            waited_clause(s(0.05), s(0.0)).contains("0.1s"),
            "50 ms shows"
        );
        let lock = waited_clause(s(7.0), s(0.049));
        assert!(lock.contains("7.0s") && !lock.contains("0.0s"), "{lock}");
        let core = waited_clause(s(0.0), s(340.2));
        assert!(core.contains("340.2s") && !core.contains("0.0s"), "{core}");
        let both = waited_clause(s(12.0), s(3.5));
        assert!(both.contains("12.0s") && both.contains("3.5s"), "{both}");
        assert_ne!(
            waited_clause(s(7.0), s(0.0)),
            waited_clause(s(0.0), s(7.0)),
            "which wait it was"
        );
    }

    #[test]
    fn an_in_process_walk_answers_or_is_named_unfinished() {
        assert_eq!(bounded(Some(DEFAULT_CHILD_CEILING), || 7), Ok(7));
        assert_eq!(bounded(None, || 7), Ok(7));
        let panicked = bounded(None, || -> u8 { panic!("a walk that panics") });
        assert_eq!(panicked, Err(Unfinished::Panicked));
        let (_hold, never) = std::sync::mpsc::channel::<()>();
        let ceiling = Duration::from_millis(50);
        let hung = bounded(Some(ceiling), move || never.recv().ok());
        assert_eq!(hung, Err(Unfinished::TimedOut(ceiling)));
    }

    #[cfg(unix)]
    #[test]
    fn compiler_progress_child_stand_in() {
        let Some(dir) = std::env::var_os("ATERM_VERIFY_COMPILER_PROGRESS_CHILD") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
        let dispatch = capture.lines().next().unwrap();
        let artifact = capture.lines().nth(1).unwrap();
        println!("\nBEGIN OWNED COMPILER PAYLOAD\n{dispatch}\n{artifact}");
        eprintln!(
            r#"{{"reason":"compiler-artifact","target":{{"name":"stderr_only","kind":["lib"]}}}}"#
        );
        std::io::stdout().flush().unwrap();
        std::fs::write(dir.join("ready"), b"ready").unwrap();
        let until = Instant::now() + Duration::from_secs(60);
        while !dir.join("release").exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            dir.join("release").exists(),
            "compiler fixture was never released"
        );
        println!("{}", dispatch.replace("\"changed-env\"", "\"forced\""));
        println!("exact compiler suffix\nEND OWNED COMPILER PAYLOAD");
        if std::env::var("ATERM_VERIFY_COMPILER_PROGRESS_FAIL").as_deref() == Ok("1") {
            panic!("owned compiler failure after its full payload");
        }
    }

    #[cfg(unix)]
    #[test]
    fn compiler_progress_is_observed_before_owned_release_without_final_join_authority() {
        for (step, fails) in [
            (Some("test compile"), false),
            (Some("test compile"), true),
            (None, false),
        ] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let path = dir.path().join("events");
            let events = crate::event_log::EventLog::open(&path).unwrap();
            let cmd = Cmd::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "exec::tests::compiler_progress_child_stand_in",
                    "--nocapture",
                ])
                .env(
                    "ATERM_VERIFY_COMPILER_PROGRESS_CHILD",
                    dir.path().as_os_str(),
                )
                .env(
                    "ATERM_VERIFY_COMPILER_PROGRESS_FAIL",
                    if fails { "1" } else { "0" },
                );
            let (result, held, (early, early_status)) = std::thread::scope(|scope| {
                let run = scope.spawn(|| {
                    run_machine(
                        &cmd,
                        ExecEnv {
                            events: Some(&events),
                            ..ceiled(dir.path(), None)
                        },
                        step,
                    )
                });
                let until = Instant::now() + Duration::from_secs(60);
                let early = loop {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    let observed: Vec<aterm_json::Value> = text
                        .split_inclusive('\n')
                        .filter_map(|line| line.strip_suffix('\n'))
                        .filter_map(|line| line.strip_prefix("verify-event: "))
                        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
                        .filter(|event| event["event"].as_str() == Some("compile-message-observed"))
                        .collect();
                    let both = ["dispatch", "artifact"].iter().all(|kind| {
                        observed
                            .iter()
                            .any(|event| event["message_kind"].as_str() == Some(*kind))
                    });
                    if (dir.path().join("ready").exists() && (step.is_none() || both))
                        || Instant::now() >= until
                    {
                        let status = crate::status::observe(&text);
                        break (text, status);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                };
                let held = dir.path().join("ready").exists() && !run.is_finished();
                // All assertions happen after the exact owned child was released and joined.
                std::fs::write(dir.path().join("release"), b"release").unwrap();
                (run.join().unwrap(), held, early)
            });
            assert!(held);
            assert_eq!(result.run.ok, !fails, "{}", result.run.output);
            assert_eq!(result.run.code, Some(if fails { 101 } else { 0 }));
            assert!(result.run.stopped_at.is_none());
            let stdout = result.stdout.unwrap();
            let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
            let dispatch = capture.lines().next().unwrap();
            let artifact = capture.lines().nth(1).unwrap();
            let payload = stdout
                .split_once("BEGIN OWNED COMPILER PAYLOAD\n")
                .unwrap()
                .1
                .split_once("END OWNED COMPILER PAYLOAD\n")
                .unwrap()
                .0;
            assert_eq!(
                payload,
                format!(
                    "{dispatch}\n{artifact}\n{}\nexact compiler suffix\n",
                    dispatch.replace("\"changed-env\"", "\"forced\"")
                )
            );
            assert!(!stdout.contains("stderr_only"));
            assert!(result.run.output.contains("stderr_only"));
            if step.is_some() {
                assert!(early.contains("compile-message-observed"));
                assert!(!early.contains("stderr_only"));
                assert!(!early.contains("/Users/"));
                assert!(!early.contains("compile-unit-finished"));
                let rows: Vec<aterm_json::Value> = early
                    .lines()
                    .map(|line| {
                        aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap()
                    })
                    .collect();
                assert!(
                    rows.iter()
                        .all(|row| row["stage"].as_str() == Some("test compile")
                            && row["provisional"].as_bool() == Some(true)
                            && row["sample_complete"].as_bool() == Some(false))
                );
                assert_eq!(early_status.compiler_message_records, 2);
                assert_eq!(early_status.compiler_progress.len(), 1);
                assert!(early_status.checks.is_empty());
                assert_eq!(early_status.terminal_records, 0);
                let status = early_status.json();
                assert_eq!(
                    status["compiler_progress"][0]["latest"]["message_kind"].as_str(),
                    Some("artifact")
                );
                assert_eq!(
                    status["compiler_progress"][0]["provisional"].as_bool(),
                    Some(true)
                );
                assert_eq!(
                    status["compiler_progress"][0]["sample_complete"].as_bool(),
                    Some(false)
                );
                assert_eq!(early_status.invocations.len(), 1);
                assert_eq!(
                    early_status.invocations[0]
                        .observed
                        .compiler_message_records,
                    2
                );
            } else {
                assert!(early.is_empty());
                assert!(early_status.compiler_progress.is_empty());
            }
            let final_events = crate::report::compile_unit_events("test compile", &stdout);
            let artifact: aterm_json::Value = final_events
                .iter()
                .filter_map(|line| {
                    aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ")?)
                        .ok()
                })
                .find(|row| row["event"].as_str() == Some("compile-unit-finished"))
                .unwrap();
            assert!(artifact["rebuild_reason"].is_null());
            assert_eq!(
                artifact["rebuild_reason_status"].as_str(),
                Some("conflicting-dispatch")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn evidence_child_stand_in() {
        let Some(dir) = std::env::var_os("ATERM_VERIFY_EVIDENCE_CHILD") else {
            return;
        };
        let dir = PathBuf::from(dir);
        if std::env::var("ATERM_VERIFY_EVIDENCE_PREFIX").as_deref() == Ok("1") {
            std::io::stdout().write_all(&vec![b'x'; 33 << 20]).unwrap();
        }
        // Single-threaded libtest leaves its human case prefix unterminated.
        // Start this fixture's synthetic JSON stream on its own line.
        println!();
        println!(
            "{}",
            aterm_json::json!({"type":"test", "event":"started", "name":"observed-outer"})
        );
        println!(
            "{}",
            aterm_json::json!({"type":"test", "event":"failed", "name":"observed-outer", "stdout":"thread 'outer' panicked at src/owned.rs:7:9:\nowned diagnostic"})
        );
        std::io::stdout().flush().unwrap();
        std::fs::write(dir.join("ready"), b"ready").unwrap();
        let until = Instant::now() + Duration::from_secs(60);
        while !dir.join("release").exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            dir.join("release").exists(),
            "owned evidence wait was never released"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_case_evidence_is_durable_while_its_owned_child_is_still_held() {
        for late in [false, true] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let events_path = dir.path().join("events");
            let events = crate::event_log::EventLog::open(&events_path).unwrap();
            let mut documentation = crate::ladder::Report::new("documentation fixture");
            documentation.pass("complete");
            let green = crate::report::judge(
                crate::Check::Doc,
                &[&documentation],
                0.0,
                "",
                String::new(),
                None,
            );
            events.write(&crate::report::check_event(&green, &[&documentation]));
            let mut cmd = Cmd::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "exec::tests::evidence_child_stand_in",
                    "--nocapture",
                ])
                .env("ATERM_VERIFY_EVIDENCE_CHILD", dir.path().as_os_str())
                .env("ATERM_VERIFY_EVIDENCE_PREFIX", if late { "1" } else { "0" });
            cmd.test_cases = Some(CaseWatch {
                spec: "owned-test".into(),
                binary: "owned-evidence-child".into(),
                hang_after: TEST_BINARY_HANG,
                json: true,
                stop_at_red: false,
            });
            let (run, (held, early_text, observed, value)) = std::thread::scope(|scope| {
                let run = scope.spawn(|| {
                    run(
                        &cmd,
                        ExecEnv {
                            events: Some(&events),
                            ..ceiled(dir.path(), None)
                        },
                    )
                });
                let until = Instant::now() + Duration::from_secs(60);
                let early = loop {
                    let text = std::fs::read_to_string(&events_path).unwrap_or_default();
                    let case = text
                        .split_inclusive('\n')
                        .filter_map(|line| line.strip_suffix('\n'))
                        .filter_map(|line| line.strip_prefix("verify-event: "))
                        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
                        .find(|event| {
                            event["event"].as_str() == Some("test-case-failed")
                                && event["case"].as_str() == Some("observed-outer")
                        });
                    if (case.is_some() && dir.path().join("ready").exists())
                        || Instant::now() >= until
                    {
                        let observed = crate::status::observe(&text);
                        break (text, observed, case);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                };
                let held = dir.path().join("ready").exists() && !run.is_finished();
                // Release and join before assertions, including the old stderr-only negative.
                std::fs::write(dir.path().join("release"), b"release").unwrap();
                (run.join().unwrap(), (held, early.0, early.1, early.2))
            });
            assert!(held, "child must still own its explicit wait");
            let event = crate::libtest_json::line_event(&early_text);
            assert!(event.is_none(), "verify events are not libtest events");
            let value = value.expect("the actual complete case record was retained before release");
            assert_eq!(value["case"].as_str(), Some("observed-outer"));
            assert!(
                value["output"]
                    .as_str()
                    .unwrap()
                    .contains("src/owned.rs:7:9")
            );
            assert_eq!(value["halts"].as_bool(), Some(false));
            assert_eq!(observed.checks.len(), 1);
            assert_eq!(observed.checks[0].check, "doc");
            assert_eq!(
                observed.checks[0]
                    .outcomes
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["ok"]
            );
            assert_eq!(observed.terminal_records, 0);
            assert_eq!(observed.early_cases.len(), 1);
            assert_eq!(observed.early_cases[0].test, "owned-test");
            assert_eq!(
                observed.early_cases[0].case.as_deref(),
                Some("observed-outer")
            );
            assert_eq!(
                observed.early_cases[0]
                    .outcomes
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["FAIL"]
            );
            assert!(!observed.json().to_string().contains("src/owned.rs"));
            events.write(&crate::report::run_event(&[green], crate::exit::PASS));
            let completed = crate::status::observe(&std::fs::read_to_string(&events_path).unwrap());
            assert_eq!(completed.early_cases, observed.early_cases);
            assert_eq!(completed.terminal_consistency, "matching-observed-rows");
            assert!(run.ok, "observation does not change an actual zero exit");
            assert!(run.stopped_at.is_none());
            assert_eq!(
                run.case_observations.unwrap().failures[0].case,
                "observed-outer"
            );
        }
    }

    #[test]
    /// A completed stream's final poll reports its failure without granting it permission to stop
    /// anything. NEGATIVE CONTROLS: an empty stream and a failed event without its preceding start
    /// emit nothing. Direct fixture bytes keep this parser test independent of nested libtest's
    /// stdout/stderr write order.
    fn a_completed_failure_is_observed_without_forcing_a_stop() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        let mut capture = create_stage_log(&path).unwrap();
        let events_path = dir.path().join("events");
        let events = crate::event_log::EventLog::open(&events_path).unwrap();
        let mut cases = test_progress::LiveCases::open(&path, true).unwrap();
        let watch = CaseWatch {
            spec: "completed".into(),
            binary: "completed-fixture".into(),
            hang_after: TEST_BINARY_HANG,
            json: true,
            stop_at_red: true,
        };

        observe_reaped(&mut cases, &watch, Some(&events), Duration::ZERO);
        assert_eq!(std::fs::read_to_string(&events_path).unwrap(), "");

        let failed = aterm_json::json!({
            "type":"test", "event":"failed", "name":"observed-outer",
            "stdout":"thread 'outer' panicked at src/owned.rs:7:9:\nowned diagnostic"
        });
        writeln!(capture, "{failed}").unwrap();
        capture.flush().unwrap();
        observe_reaped(&mut cases, &watch, Some(&events), Duration::ZERO);
        assert_eq!(std::fs::read_to_string(&events_path).unwrap(), "");

        writeln!(
            capture,
            "{}",
            aterm_json::json!({"type":"test", "event":"started", "name":"observed-outer"})
        )
        .unwrap();
        writeln!(capture, "{failed}").unwrap();
        capture.flush().unwrap();
        observe_reaped(&mut cases, &watch, Some(&events), Duration::ZERO);
        let text = std::fs::read_to_string(events_path).unwrap();
        assert_eq!(text.lines().count(), 1);
        let event: aterm_json::Value =
            aterm_json::from_str(text.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(event["case"].as_str(), Some("observed-outer"));
        assert_eq!(event["halts"].as_bool(), Some(false));
        assert!(
            event["output"]
                .as_str()
                .unwrap()
                .contains("owned diagnostic")
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_output_stand_in() {
        let Some(dir) = std::env::var_os("ATERM_VERIFY_BOUNDED_OUTPUT_CHILD") else {
            return;
        };
        let dir = PathBuf::from(dir);
        println!("owned stdout");
        eprintln!("owned stderr");
        std::fs::write(dir.join("ready"), format!("{}\n", std::process::id())).unwrap();
        if std::env::var_os("ATERM_VERIFY_BOUNDED_OUTPUT_NORMAL").is_some() {
            return;
        }
        while !dir.join("stop").exists() {
            std::thread::sleep(Duration::from_millis(1));
        }
        aterm_uds::process::kill_self_now();
    }

    #[cfg(unix)]
    fn bounded_output_command(dir: &Path, normal: bool) -> aterm_uds::spawnfd::Command {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "exec::tests::bounded_output_stand_in",
                "--nocapture",
            ])
            .env("ATERM_VERIFY_BOUNDED_OUTPUT_CHILD", dir);
        if normal {
            command.env("ATERM_VERIFY_BOUNDED_OUTPUT_NORMAL", "1");
        }
        command
    }

    static BOUNDED_QUIET: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    fn bounded_quiet_admit(
        need: aterm_capacity::Need,
        mode: aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
        crate::admit_in(
            &aterm_capacity::Ledger::at(BOUNDED_QUIET.get().expect("scratch ledger")),
            &crate::timing_test_machine::Measured::new(&aterm_capacity::Fixed::new(
                aterm_capacity::Reading::of_gib(24, 20, 14),
            )),
            aterm_capacity::Pace {
                poll: Duration::from_millis(20),
                ..aterm_capacity::Pace::default()
            },
            need,
            mode,
            &mut |_| {},
        )
    }

    /// A completed last stage releases its quiet claim. A last stage whose bounded walk timed
    /// out keeps that same claim while the actual worker is blocked, even after the stage's
    /// owner is dropped, and releases it once that worker ends. The timeout is injected only
    /// after the worker entered; every wait is a hang detector, never an elapsed-time budget.
    #[test]
    fn a_last_stage_keeps_its_quiet_claim_until_its_bounded_worker_ends() {
        const HANG_DETECTOR: Duration = aterm_spec::wait::MUST_HAPPEN;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join("ledger");
        BOUNDED_QUIET.set(path.clone()).expect("set once");
        let ledger = aterm_capacity::Ledger::at(&path);
        let held = |ledger: &aterm_capacity::Ledger| {
            ledger
                .list()
                .expect("ledger")
                .iter()
                .any(|r| r.held.is_some())
        };
        let ctx = crate::Ctx::new(
            dir.path().to_owned(),
            crate::Scope::crate_only("aterm-gui"),
            crate::EnvSnapshot::default(),
            dir.path().join("scratch"),
        )
        .with_quiet(Some(aterm_capacity::Admit::Wait));
        let last_stage = || {
            std::sync::Mutex::new(crate::Quiet {
                claim: None,
                left: 1,
                admit: bounded_quiet_admit,
            })
        };
        let ordinary = last_stage();
        let worker_ledger = aterm_capacity::Ledger::at(&path);
        let (complete, _) = crate::quietly(&ctx, &ordinary, "ordinary last stage", || {
            bounded(Some(HANG_DETECTOR), move || held(&worker_ledger))
        })
        .expect("admitted");
        assert_eq!(complete, Ok(true), "ordinary worker ran while admitted");
        aterm_spec::wait::until_within(
            HANG_DETECTOR,
            "completed worker released its quiet claim",
            || (!held(&ledger)).then_some(()),
        );

        let (entered, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let last = last_stage();
        let (answer, _) = crate::quietly(&ctx, &last, "timed-out last stage", || {
            bounded_wait(
                move || {
                    entered.send(()).expect("entered");
                    resume.recv_timeout(HANG_DETECTOR).expect("released");
                    finished.send(()).expect("worker finished");
                },
                |_| {
                    ready.recv_timeout(HANG_DETECTOR).expect("worker entered");
                    Err(Unfinished::TimedOut(HANG_DETECTOR))
                },
            )
        })
        .expect("admitted");
        drop(last);
        let held_after_stage = held(&ledger);
        release.send(()).expect("release worker");
        done.recv_timeout(HANG_DETECTOR).expect("worker ended");
        aterm_spec::wait::until_within(HANG_DETECTOR, "worker released its quiet claim", || {
            (!held(&ledger)).then_some(())
        });
        assert_eq!(answer, Err(Unfinished::TimedOut(HANG_DETECTOR)));
        assert!(
            held_after_stage,
            "last stage returned but its actual worker still ran"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_bounded_walk_reaps_its_output_child_before_reporting_timeout() {
        let dir = aterm_tempfile::tempdir().unwrap();
        struct Cleanup {
            stop: PathBuf,
            done: std::sync::mpsc::Receiver<std::io::Result<std::process::Output>>,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                std::fs::write(&self.stop, "stop").unwrap();
                let output = self
                    .done
                    .recv_timeout(HANG_DETECTOR)
                    .expect("owned output worker ended");
                if let Ok(output) = output {
                    use std::os::unix::process::ExitStatusExt as _;
                    assert_eq!(output.status.signal(), Some(9), "stand-in dies by SIGKILL");
                }
            }
        }
        let (done, rx) = std::sync::mpsc::channel();
        let cleanup = Cleanup {
            stop: dir.path().join("stop"),
            done: rx,
        };
        let work = dir.path().to_owned();
        let mut pid = String::new();
        let timed_out: Result<(), _> = bounded_wait(
            move || {
                let output = aterm_uds::output::output(&mut bounded_output_command(&work, false));
                done.send(output).unwrap();
            },
            |_| {
                pid = read_pid(&dir.path().join("ready"));
                assert!(
                    aterm_uds::process::pid_alive(pid.parse().unwrap()),
                    "actual stand-in is live"
                );
                Err(Unfinished::TimedOut(HANG_DETECTOR))
            },
        );
        assert_eq!(timed_out, Err(Unfinished::TimedOut(HANG_DETECTOR)));
        let absent = !aterm_uds::process::pid_alive(pid.parse().unwrap());
        drop(cleanup);
        assert!(
            absent,
            "bounded returned with its output child {pid} still alive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_bounded_walk_keeps_normal_child_output() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let work = dir.path().to_owned();
        let output = bounded(Some(HANG_DETECTOR), move || {
            aterm_uds::output::output(&mut bounded_output_command(&work, true))
        })
        .unwrap()
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("owned stdout"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("owned stderr"));
    }

    #[cfg(unix)]
    #[test]
    fn a_timed_out_walk_cannot_launch_a_child_later() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let work = dir.path().to_owned();
        let (parked, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let (sent, result) = std::sync::mpsc::channel();
        let timed_out: Result<(), _> = bounded_wait(
            move || {
                parked.send(()).unwrap();
                resume
                    .recv_timeout(HANG_DETECTOR)
                    .expect("release the owned walk");
                sent.send(aterm_uds::output::output(&mut bounded_output_command(
                    &work, true,
                )))
                .unwrap();
            },
            |_| {
                ready
                    .recv_timeout(HANG_DETECTOR)
                    .expect("walk parked before launching");
                Err(Unfinished::TimedOut(HANG_DETECTOR))
            },
        );
        assert_eq!(timed_out, Err(Unfinished::TimedOut(HANG_DETECTOR)));
        release.send(()).unwrap();
        let output = result
            .recv_timeout(HANG_DETECTOR)
            .expect("late launch answered");
        assert_eq!(output.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(
            !dir.path().join("ready").exists(),
            "the actual child never started"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_timed_out_walk_cannot_launch_from_its_returned_value_drop() {
        struct OutputOnDrop {
            work: PathBuf,
            sent: std::sync::mpsc::Sender<std::io::Result<std::process::Output>>,
        }
        impl Drop for OutputOnDrop {
            fn drop(&mut self) {
                let output =
                    aterm_uds::output::output(&mut bounded_output_command(&self.work, true));
                self.sent.send(output).unwrap();
            }
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let work = dir.path().to_owned();
        let (parked, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let (sent, result) = std::sync::mpsc::channel();
        let timed_out = bounded_wait(
            move || {
                parked.send(()).unwrap();
                resume
                    .recv_timeout(HANG_DETECTOR)
                    .expect("release the owned walk");
                OutputOnDrop { work, sent }
            },
            |_| {
                ready
                    .recv_timeout(HANG_DETECTOR)
                    .expect("walk parked before returning");
                Err(Unfinished::TimedOut(HANG_DETECTOR))
            },
        );
        assert!(matches!(timed_out, Err(Unfinished::TimedOut(limit)) if limit == HANG_DETECTOR));
        release.send(()).unwrap();
        let output = result
            .recv_timeout(HANG_DETECTOR)
            .expect("returned value was dropped");
        assert_eq!(output.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(
            !dir.path().join("ready").exists(),
            "the actual child never started"
        );
    }

    #[cfg(unix)]
    fn spawned_argv(c: &Command) -> Vec<String> {
        std::iter::once(c.get_program())
            .chain(c.get_args())
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn only_a_demoted_child_that_can_run_is_spawned_under_utility_qos() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::mktemp_dir("atv-qos").expect("mktemp");
        let fake = tmp.join("taskpolicy");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let fake_s = fake.to_string_lossy().into_owned();
        let env = env_in(&tmp);
        let tool = Cmd::new("/bin/sh").args(["-c", "exit 0"]);
        let desk = |idle: Option<Duration>| {
            let host: &'static Idle = Box::leak(Box::new(Idle(idle)));
            ExecEnv { host, ..env }
        };
        let wrapped = [fake_s.as_str(), "-c", "utility", "/bin/sh", "-c", "exit 0"];
        for (idle, demoted, why) in [
            (Some(Duration::from_secs(5)), true, "a person typed 5 s ago"),
            (
                Some(AWAY_AFTER - Duration::from_secs(1)),
                true,
                "just inside the bound",
            ),
            (
                None,
                true,
                "an input clock that cannot be read keeps the demotion",
            ),
            (Some(AWAY_AFTER), false, "nobody at the desk for the bound"),
            (
                Some(Duration::from_secs(10 * 3600)),
                false,
                "nobody for ten hours",
            ),
        ] {
            let argv = spawned_argv(&spawn_command(&tool.clone().demoted(), desk(idle), &fake));
            if demoted {
                assert_eq!(argv, wrapped, "{why}");
            } else {
                assert_eq!(argv, ["/bin/sh", "-c", "exit 0"], "{why}");
            }
        }

        assert_eq!(
            spawned_argv(&spawn_command(&tool, env, &fake)),
            ["/bin/sh", "-c", "exit 0"],
            "a child nobody demoted keeps the inherited tier"
        );
        assert_eq!(
            spawned_argv(&spawn_command(&tool.clone().demoted(), env, &fake)),
            [fake_s.as_str(), "-c", "utility", "/bin/sh", "-c", "exit 0"]
        );
        assert_eq!(
            spawned_argv(&spawn_command(
                &tool.clone().demoted(),
                env,
                &tmp.join("absent")
            ))[0],
            "/bin/sh",
            "no taskpolicy on this machine: the tool alone"
        );
        let missing = Cmd::new(tmp.join("no-such-tool")).demoted();
        assert_eq!(
            spawned_argv(&spawn_command(&missing, env, &fake))[0],
            missing.program.to_string_lossy(),
            "a missing tool is spawned bare, so it still fails to SPAWN"
        );
        assert_eq!(
            spawned_argv(&spawn_command(&Cmd::new("sh").demoted(), env, &fake))[0],
            fake_s,
            "a bare name resolves on the child's PATH"
        );
        let hidden = Cmd::new("sh").env("PATH", tmp.as_os_str()).demoted();
        assert_eq!(
            spawned_argv(&spawn_command(&hidden, env, &fake))[0],
            "sh",
            "the child's own PATH override is the one searched"
        );
        let loader = tool
            .clone()
            .env("DYLD_FALLBACK_LIBRARY_PATH", "/tc/lib:/usr/lib")
            .env("RUSTC", "/tc/bin/rustc")
            .demoted();
        let argv = spawned_argv(&spawn_command(&loader, env, &fake));
        if crate::is_executable_file(Path::new(ENV)) {
            assert_eq!(
                argv,
                [
                    fake_s.as_str(),
                    "-c",
                    "utility",
                    ENV,
                    "DYLD_FALLBACK_LIBRARY_PATH=/tc/lib:/usr/lib",
                    "/bin/sh",
                    "-c",
                    "exit 0"
                ]
            );
        }
        assert_eq!(
            spawned_argv(&spawn_command(
                &loader.clone().env("X", "1"),
                env,
                &tmp.join("absent")
            )),
            ["/bin/sh", "-c", "exit 0"],
            "no taskpolicy: the variables stay the Command's own"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "exec_custody_tests.rs"]
mod custody_tests;

#[cfg(test)]
#[path = "exec_memory_tests.rs"]
mod memory_tests;
