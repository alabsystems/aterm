// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flag surface, which is FROZEN.
//!
//! `--fast` / `--full` / `--scope <crate>` keep working unedited, including
//! `--scope=<crate>` and `-h`/`--help`. (`--selftest` and `--in-place` are gone
//! since 2026-09-27: every run is a real run of a snapshot.)
//!
//! **THE GATE NEEDS NO FLAG** (2026-09-23, the owner, reading
//! `tools/verify.sh --fast` in a report: *"why fucking --fast? I always want
//! 'fast'"*). Fast has always been [`Mode`]'s default — a bare
//! `tools/verify.sh` IS the merge contract — but every instruction in the repo
//! spelled the flag, which read as if something slower ran without it. The
//! docs and the usage below now spell the bare command; `--fast` stays
//! accepted, a no-op, so a script that types it keeps working.
//!
//! **`--measure` IS THE ONE MODE ADDED SINCE** (2026-09-26): the MEASURE tier
//! ([`crate::plan::Tier`]) — the stages that measure the machine or the release
//! artifact — left the default ladder, so the merge contract stopped judging a
//! push by how busy the box was, and a release cut requires it instead.
//!
//! One addition for the bash shim that execs this driver: `--root <dir>` (the
//! shim already resolved the repo root from its own path, and a compiled binary
//! cannot). It changes no stage's decision.
//!
//! THE GATE READS NO ENVIRONMENT KNOB OF ITS OWN (2026-09-24). Every setting a
//! person gives the gate is a flag here — `--stage-timeout`, `--test-threads`,
//! `--test-jobs`, `--snapshot`, `--log`/`--no-log`, `--skip-gui-smoke`,
//! `--machine-lock-dir` — because this crate links into the shipped `aterm`
//! (`aterm help rust`), where the owner's rule admits no environment variable
//! that changes what it does (`aterm-update-core`'s `env_reads` gate).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

/// Which TIERS a run runs ([`crate::plan::Tier`]).
///
/// * `Fast` — the LAND tier, the per-commit merge contract: the DEFAULT, what
///   a bare `tools/verify.sh` runs (`--fast` spells it and changes nothing).
/// * `Measure` — `--measure` (2026-09-26): the MEASURE tier alone — the
///   release artifact's paint and spin matrices and the build they judge, and
///   the typing-pacing smoke. Not the merge contract; what a release cut
///   requires green for the tree it cuts.
/// * `Full` — `--full`: both tiers, plus the trust-mc / Kani floor, the
///   cross-cell type-check, the startup comparison and the Codex live upgrade.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Fast,
    Measure,
    Full,
}

impl Mode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Fast => "fast",
            Mode::Measure => "measure",
            Mode::Full => "full",
        }
    }

    /// Does this mode run `tier`? The LAND tier is every mode's but
    /// `--measure`'s, the MEASURE tier is `--measure`'s and `--full`'s, and
    /// the `--full`-only stages are `--full`'s alone.
    #[must_use]
    pub fn runs(self, tier: crate::plan::Tier) -> bool {
        use crate::plan::Tier;
        match tier {
            Tier::Land => self != Mode::Measure,
            Tier::Measure => self != Mode::Fast,
            Tier::Full => self == Mode::Full,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Args {
    pub mode: Mode,
    /// `None` = the whole workspace.
    pub scope: Option<String>,
    /// `--changed`: narrow to the diff's crates and their dependents.
    pub changed: bool,
    /// `--base <ref>` AS GIVEN. `None` means "not given", which is not the same
    /// as "given the default": only the first may pass the `--changed` check
    /// below, so `--base main` without `--changed` is still the usage error it
    /// always was in intent.
    pub base: Option<String>,
    pub root: Option<PathBuf>,
    /// `--disk-floor <GiB>`: the free space this run requires, in whole GiB,
    /// in place of the disk preflight's estimate. `None` keeps the estimate
    /// ([`crate::disk::Budget::need`]). The gate's own integration tests pass
    /// `0`: a fixture with a fake toolchain builds nothing, and a fixed floor
    /// made the gate's tests refuse whenever the host volume held less than it
    /// (measured 2026-09-23 at 17.6 GiB free: 23 tests).
    pub disk_floor_gib: Option<u64>,
    /// `--stage-timeout <seconds|off>`: the wall-clock ceiling on one stage
    /// child. `None` keeps [`crate::exec::DEFAULT_CHILD_CEILING`]; `Some(None)` is
    /// `off`, the unbounded wait ([`crate::exec::parse_ceiling`]).
    pub stage_timeout: Option<Option<Duration>>,
    /// `--test-threads <n>`: the `RUST_TEST_THREADS` every child is pinned to.
    /// `None` pins the machine's parallelism ([`crate::Ctx::with_pinned_child_facts`]).
    pub test_threads: Option<NonZeroU32>,
    /// `--test-jobs <n>`: how many test binaries the test stage runs at once,
    /// splitting the pinned `RUST_TEST_THREADS` between them ([`crate::testrun`]).
    /// `None` keeps [`crate::testrun::DEFAULT_JOBS`]; `1` is one at a time.
    pub test_jobs: Option<NonZeroU32>,
    /// `--snapshot <dir>`: where the snapshot lives, in place of
    /// `<root>-verify.noindex` ([`crate::snapshot`]).
    pub snapshot: Option<PathBuf>,
    /// `--log <path>`: where the gate writes its own copy of the ladder, in
    /// place of `<root>/.aterm-verify/logs/verify-<pid>.log`.
    pub log: Option<PathBuf>,
    /// `--no-log`: the gate keeps no copy of the ladder.
    pub no_log: bool,
    /// `--skip-gui-smoke`: the GUI smoke SKIPS — a named skip, so the run is
    /// refused the merge-contract verdict and its receipt vouches for nothing.
    pub skip_gui_smoke: bool,
    /// `--machine-lock-dir <dir>`: where the one-gate-per-machine lock lives, in
    /// place of the per-user one ([`crate::snapshot::machine_lock_dir`]). The
    /// gate's own fixture tests give every throwaway repo its own.
    pub machine_lock_dir: Option<PathBuf>,
    /// `--baseline`: record MAIN's own reds — HEAD a clean commit of
    /// `origin/main`, the whole tree — and publish the receipt for branches to
    /// be judged against ([`crate::differential`]).
    pub baseline: bool,
    pub help: bool,
}

impl Args {
    /// The ref the diff is taken against: `--base`, else `main`.
    #[must_use]
    pub fn base_ref(&self) -> String {
        self.base.clone().unwrap_or_else(|| "main".to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    ScopeNeedsCrateName,
    BaseNeedsGitRef,
    RootNeedsPath,
    /// `--scope` and `--changed` are two different narrowings.
    ScopeAndChanged,
    /// `--base` without `--changed` narrows nothing and means nothing.
    BaseWithoutChanged,
    /// `--disk-floor` with no value, or one that is not a whole number of GiB.
    DiskFloorNeedsGib,
    /// `--stage-timeout` with no value, or one that is neither seconds nor `off`.
    StageTimeoutNeedsSeconds,
    /// `--test-threads` with no value, or one that is not a positive count.
    TestThreadsNeedsCount,
    /// `--test-jobs` with no value, or one that is not a positive count.
    TestJobsNeedsCount,
    /// A path flag (`--snapshot`, `--log`, `--machine-lock-dir`) with no value.
    NeedsPath(String),
    /// `--log <path>` and `--no-log` together.
    LogAndNoLog,
    /// `--baseline` with `--scope` or `--changed`: a baseline is main's reds
    /// over the WHOLE tree, and a narrowed run records none.
    BaselineNarrowed,
    /// `--baseline` with `--measure`: a baseline is main's reds for the merge
    /// contract, and a `--measure` run runs no stage of it.
    BaselineMeasure,
    Unknown(String),
}

impl ParseError {
    /// The message the script wrote to stderr, unchanged where it existed.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            ParseError::ScopeNeedsCrateName => "verify: --scope needs a crate name".to_string(),
            ParseError::BaseNeedsGitRef => "verify: --base needs a git ref".to_string(),
            ParseError::RootNeedsPath => "verify: --root needs a directory".to_string(),
            ParseError::ScopeAndChanged => {
                "verify: --scope and --changed both narrow the build; pick one".to_string()
            }
            ParseError::BaseWithoutChanged => {
                "verify: --base <ref> only means something with --changed".to_string()
            }
            ParseError::DiskFloorNeedsGib => {
                "verify: --disk-floor needs a whole number of GiB".to_string()
            }
            ParseError::StageTimeoutNeedsSeconds => {
                "verify: --stage-timeout needs a number of seconds, or off".to_string()
            }
            ParseError::TestThreadsNeedsCount => {
                "verify: --test-threads needs a positive whole number".to_string()
            }
            ParseError::TestJobsNeedsCount => {
                "verify: --test-jobs needs a positive whole number".to_string()
            }
            ParseError::NeedsPath(flag) => format!("verify: {flag} needs a path"),
            ParseError::LogAndNoLog => {
                "verify: --log <path> and --no-log contradict each other; pick one".to_string()
            }
            ParseError::BaselineNarrowed => "verify: --baseline records main's reds over the \
                                             whole tree; it cannot be narrowed (--scope, \
                                             --changed)"
                .to_string(),
            ParseError::BaselineMeasure => "verify: --baseline records main's reds for the merge \
                                            contract; --measure runs the MEASURE tier alone, \
                                            which is not part of it"
                .to_string(),
            ParseError::Unknown(a) => format!("verify: unknown argument: {a}"),
        }
    }
}

/// Parse the command line.
///
/// # Errors
/// Returns the usage error to print before exiting `2`.
pub fn parse<I, S>(args: I) -> Result<Args, ParseError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut out = Args::default();
    let mut it = args.into_iter().map(Into::into);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--fast" => out.mode = Mode::Fast,
            "--full" => out.mode = Mode::Full,
            "--measure" => out.mode = Mode::Measure,
            "--changed" => out.changed = true,
            "--no-log" => out.no_log = true,
            "--skip-gui-smoke" => out.skip_gui_smoke = true,
            "--baseline" => out.baseline = true,
            "-h" | "--help" => out.help = true,
            "--scope" => {
                let v = it.next().unwrap_or_default();
                if v.is_empty() {
                    return Err(ParseError::ScopeNeedsCrateName);
                }
                out.scope = Some(v);
            }
            "--base" => {
                let v = it.next().unwrap_or_default();
                if v.is_empty() {
                    return Err(ParseError::BaseNeedsGitRef);
                }
                out.base = Some(v);
            }
            "--root" => {
                let v = it.next().unwrap_or_default();
                if v.is_empty() {
                    return Err(ParseError::RootNeedsPath);
                }
                out.root = Some(PathBuf::from(v));
            }
            "--disk-floor" => {
                out.disk_floor_gib = Some(parse_gib(&it.next().unwrap_or_default())?);
            }
            "--stage-timeout" => {
                out.stage_timeout = Some(parse_stage_timeout(&it.next().unwrap_or_default())?);
            }
            "--test-threads" => {
                out.test_threads = Some(parse_threads(&it.next().unwrap_or_default())?);
            }
            "--test-jobs" => {
                out.test_jobs = Some(parse_jobs(&it.next().unwrap_or_default())?);
            }
            "--snapshot" | "--log" | "--machine-lock-dir" => {
                let v = path_value(&a, it.next().unwrap_or_default())?;
                *out.path_slot(&a) = Some(v);
            }
            _ => {
                if let Some(v) = a.strip_prefix("--scope=") {
                    // FAIL-CLOSED DIVERGENCE, deliberate. Bash let `--scope=`
                    // set an EMPTY scope, which meant "the whole workspace" —
                    // so a typo silently WIDENED the claim all the way to the
                    // merge-contract sentence. That is precisely the class of
                    // false green the verdict discipline exists to stop.
                    if v.is_empty() {
                        return Err(ParseError::ScopeNeedsCrateName);
                    }
                    out.scope = Some(v.to_string());
                } else if let Some(v) = a.strip_prefix("--base=") {
                    // Same fail-closed rule as `--scope=`: bash let `--base=`
                    // set an EMPTY ref, which then failed to find a merge-base
                    // and WIDENED the run without the caller ever learning the
                    // flag was malformed.
                    if v.is_empty() {
                        return Err(ParseError::BaseNeedsGitRef);
                    }
                    out.base = Some(v.to_string());
                } else if let Some(v) = a.strip_prefix("--root=") {
                    if v.is_empty() {
                        return Err(ParseError::RootNeedsPath);
                    }
                    out.root = Some(PathBuf::from(v));
                } else if let Some(v) = a.strip_prefix("--disk-floor=") {
                    out.disk_floor_gib = Some(parse_gib(v)?);
                } else if let Some(v) = a.strip_prefix("--stage-timeout=") {
                    out.stage_timeout = Some(parse_stage_timeout(v)?);
                } else if let Some(v) = a.strip_prefix("--test-threads=") {
                    out.test_threads = Some(parse_threads(v)?);
                } else if let Some(v) = a.strip_prefix("--test-jobs=") {
                    out.test_jobs = Some(parse_jobs(v)?);
                } else if let Some((flag, v)) =
                    a.split_once('=').filter(|(f, _)| PATH_FLAGS.contains(f))
                {
                    let v = path_value(flag, v.to_string())?;
                    *out.path_slot(flag) = Some(v);
                } else {
                    return Err(ParseError::Unknown(a));
                }
            }
        }
    }
    // Two different narrowings: silently letting one win would make the printed
    // scope a lie about what was built.
    if out.scope.is_some() && out.changed {
        return Err(ParseError::ScopeAndChanged);
    }
    if out.base.is_some() && !out.changed {
        return Err(ParseError::BaseWithoutChanged);
    }
    if out.log.is_some() && out.no_log {
        return Err(ParseError::LogAndNoLog);
    }
    if out.baseline && (out.scope.is_some() || out.changed) {
        return Err(ParseError::BaselineNarrowed);
    }
    if out.baseline && out.mode == Mode::Measure {
        return Err(ParseError::BaselineMeasure);
    }
    Ok(out)
}

/// The flags whose value is a path ([`path_value`]).
const PATH_FLAGS: [&str; 3] = ["--snapshot", "--log", "--machine-lock-dir"];

impl Args {
    /// The field a path flag ([`PATH_FLAGS`]) sets.
    fn path_slot(&mut self, flag: &str) -> &mut Option<PathBuf> {
        match flag {
            "--snapshot" => &mut self.snapshot,
            "--machine-lock-dir" => &mut self.machine_lock_dir,
            _ => &mut self.log,
        }
    }
}

/// A path flag's value: an empty one is a usage error, never read as "unset" — and so
/// is a FLAG. `tools/verify.sh` appends `--root <dir>` after the caller's words, so a
/// trailing `--log` with its path forgotten would otherwise take `--root` as the path.
fn path_value(flag: &str, v: String) -> Result<PathBuf, ParseError> {
    if v.is_empty() || v.starts_with("--") {
        return Err(ParseError::NeedsPath(flag.to_string()));
    }
    Ok(PathBuf::from(v))
}

/// A `--stage-timeout` value ([`crate::exec::parse_ceiling`]); anything it cannot
/// read is a usage error — never the unbounded wait, which has to be TYPED.
fn parse_stage_timeout(v: &str) -> Result<Option<Duration>, ParseError> {
    crate::exec::parse_ceiling(v).ok_or(ParseError::StageTimeoutNeedsSeconds)
}

/// A `--test-threads` value: a positive whole number.
fn parse_threads(v: &str) -> Result<NonZeroU32, ParseError> {
    v.trim()
        .parse::<NonZeroU32>()
        .map_err(|_| ParseError::TestThreadsNeedsCount)
}

/// A `--test-jobs` value: a positive whole number.
fn parse_jobs(v: &str) -> Result<NonZeroU32, ParseError> {
    v.trim()
        .parse::<NonZeroU32>()
        .map_err(|_| ParseError::TestJobsNeedsCount)
}

/// The `--help` text: the script's own header, kept as the contract it
/// documents. `{CEILING}` is substituted by [`usage`] from
/// [`crate::exec::DEFAULT_CHILD_CEILING`] — print [`usage`], never this. The
/// ceiling was a hand-typed "45-minute" here through two raises of that
/// constant (45 → 90 min, 90 min → 3 h) and was wrong for both, so the disk
/// preflight's two numbers, `{DISK_COLD_NEED}` and `{DISK_LANE_CAP}`, are
/// substituted from [`crate::disk::Budget::MEASURED`] the same way.
pub const USAGE_TEMPLATE: &str = "\
verify — the single local gate entrypoint for aterm.

aterm has NO CI by owner decision (docs/AUDIT.md, docs/PROCESS.md): the merge
contract is *this gate passing locally* before a slice enters the ff-only
main merge-queue. There is exactly one way to verify, so there is exactly one
way for a reviewer (human or AI) to be wrong about it: run this.

  tools/verify.sh                   # the per-commit gate (the merge contract)
  tools/verify.sh --measure         # the MEASURE tier (a release cut requires it)
  tools/verify.sh --full            # both tiers + trust-mc + cross-cells
                                    #   + the Codex live upgrade
  tools/verify.sh --changed         # change-scoped tier (NOT the merge contract)
  tools/verify.sh --scope <crate>   # narrow the test compile, test run,
                                    #   deadline and measuring tests, doctests
                                    #   and lint to one crate (+ guards)
  tools/verify.sh --scope aterm-grid
  tools/verify.sh --baseline        # on a commit of main: record + publish main's reds

(no flag) : THE GATE, and the merge contract: the LAND tier — targo test
            --workspace and its doctests + the deadline tests (run alone) +
            tippy + formatting + the zero-tolerance grep guards and license
            headers + the delivery-tooling suites (installer, cargo pin,
            export policy, release preflight, site sync, dev signing identity,
            the atpkg index and publish producers) + the atpkg end-to-end pack
            + the L0 temporal-safety gate + gate forge and gate cells-foreign
            (the cells no fleet box hosts, each for its own triple, which needs
            rustup's `stable` with the four foreign std targets or is a SKIP
            that withholds the merge contract) + a headless control-socket
            smoke (the AI-first spine must never regress, so every gate run
            proves the socket still answers) + the foreground handback lane,
            which drives a private headless aterm. It does not run the MEASURE
            tier, and its verdict names every stage it left out under `MEASURE
            tier: not part of the merge contract` — never a silent skip.
--fast    : the default, spelled out. It changes nothing and nothing needs it;
            it is accepted so a script that types it keeps working.
--measure : the MEASURE tier alone: the stages that measure the machine or the
            release artifact rather than decide correctness — the fat-LTO
            release build of aterm and the paint and spin matrices that judge
            it, and the gui typing-pacing smoke (a real window's latencies).
            NOT the merge contract. Its receipt is filed apart (measure-<commit>,
            measure-tree-<tree>) and says `measured yes` only when every
            MEASURE stage ran over the whole tree and was green with nothing
            skipped; a release cut refuses to claim a build number without one
            for the tree it cuts.
--full    : both tiers — everything the default and --measure run — PLUS the
            sealed fabric rung (aterm-link --features sealed, a transport no
            shipped binary carries), the lint of the required-features
            targets, the trust-mc / Kani BMC
            harnesses *when those tools are installed* (skipped-not-failed when
            absent — see docs/PROCESS.md), the cross-cell type-check (every
            forge cell, each for its own triple; ~19 s warm and ~106 s cold
            when the matrix had five cells) and the startup-comparison
            harness's own test, PLUS — last, run alone, ~10 min — the Codex
            live upgrade, which reads THIS machine's managed store and the
            vendor's current Codex, so it is not in the per-commit contract (a
            named skip when the store holds no older Codex). Its receipt is
            filed as a merge-contract receipt AND as a MEASURE one.
--scope   : restrict the targo test/doctest/lint to `-p <crate>`; the guards
            and the socket smoke always run whole-tree (they are cheap and
            global).
--changed : a change-scoped PRE-FLIGHT. Restricts test/doctest/lint to
            the crates this branch touches PLUS every workspace crate that
            depends on one of them (the reverse-dependency cone, read from the
            SAME dependency graph the build uses). `--base <ref>` (default
            `main`) picks the merge-base the diff is taken against. Every
            whole-tree stage still runs. A narrowed run never
            claims the merge contract and its receipt vouches for nothing: other
            crates' tests read files no dependency edge names. If the scope
            cannot be computed honestly the run WIDENS to the whole workspace
            and is then judged as a whole-tree run.
            A --scope or --changed run says so BEFORE any stage runs (a
            `verify: NARROWED` line), and its receipt names the narrowing:
            `scope crate:<crate>` or `scope changed:<base>`.

JUDGED AGAINST MAIN: a run's reds are compared with main's receipt for its
            BASE — the merge-base with origin/main (for a branch that merged
            main, the main commit it merged) — looked up in the receipt store
            every worktree shares, then in the git note refs/notes/aterm-verify
            (fetched from origin when the store has none; a HEAD holding a
            local main commit origin/main lacks has no base). A red main lists
            with the same failure (same test, every character it printed —
            only where the run happened, thread ids, pids, measured durations,
            build hashes, frame addresses and commit ids masked) is INHERITED:
            named, not blocking. Any other red is NEW and blocks, and so is a
            hang, a crash (even after its test result), a log that cannot
            account for its failures, and a red that reads as a clock running
            out. The verdict says `N new, M
            inherited (red on main since <sha>)`; a run whose every red is
            inherited may claim the merge contract, and its receipt names them
            (`inherited` lines). An inherited red main has had for more than
            24 h blocks again. No usable receipt for the base: every red
            counts, as it always did, and the ladder says why before any stage.
--baseline: run on a clean commit of origin/main (a spare worktree at
            `git switch --detach origin/main`), the whole tree: records main's
            own reds — when main first went red on each is carried from the
            nearest earlier receipt on main, after fetching main's published
            notes — and publishes the receipt as a git note under
            refs/notes/aterm-verify at origin, so branches on any machine are
            judged against it. Refused (COULD NOT RUN, no receipt) on a dirty
            tree, a HEAD that is not main's, or notes it cannot fetch, and a
            usage error with --measure, which runs no stage of the merge
            contract.
            A --measure run is judged by the absolute rule: nothing is
            inherited into the MEASURE tier.

--snapshot <dir>: every run verifies a SNAPSHOT — a git worktree at
            <root>-verify.noindex (or this dir) synced to this checkout's HEAD,
            uncommitted diff and untracked files — so a pull, an edit or
            another build in this checkout cannot change what the run is
            verifying. A root git cannot open is COULD NOT RUN (exit 3). A
            compiler or source tree that moves mid-run stops every stage not
            yet started and adds a source identity COULD NOT RUN row, so a run
            with nothing failed ends COULD NOT RUN (exit 3) and a stage that
            already FAILED keeps FAIL (exit 1).

--disk-floor <GiB>: before anything is built the run budgets what it will
            write and answers COULD NOT RUN (exit 3), with no receipt, when the
            volume holding its root has less free than max(cold footprint -
            what a snapshot's build dirs already hold, warm growth) + a reserve
            — {DISK_COLD_NEED} from empty build dirs. A snapshot's build dirs over
            {DISK_LANE_CAP} are removed first and the run is budgeted cold. The
            ladder's `verify: disk …` line prints the free space, the build dirs
            and the requirement with its terms. --disk-floor replaces the estimate
            with exactly this many GiB free, and the line says so. Lowering it
            can never make a receipt lie: a run that does run out of space is
            COULD NOT RUN, never a verdict about the tree. The gate's own tests
            pass 0.

--test-threads <n>: the RUST_TEST_THREADS every child is pinned to — the
            machine's parallelism unless this names another count, never the
            invoking shell's, so the contract measures the same thing whoever
            typed the command. The run's notes say which.

--test-jobs <n>: how many test binaries the test stage runs at once (default
            {TEST_JOBS}), each with RUST_TEST_THREADS divided between them; the
            few that cannot share the machine run alone first with all of it.
            cargo records how it would start each binary and the gate runs
            them, printing their logs in cargo's order; 1 is one at a time.

--log <path> | --no-log: the gate keeps its own copy of the ladder under
            <root>/.aterm-verify/logs (the newest 20); --log moves it and
            --no-log keeps none.

--skip-gui-smoke: skip the windowed latency smoke, a MEASURE-tier stage (the
            default gate does not run it). A NAMED skip: a --full run is refused
            the merge-contract verdict, and no receipt it leaves says `measured
            yes`.

--machine-lock-dir <dir>: one gate runs per machine — a second waits (up to
            three hours) on a per-user lock under ~/Library/Caches/aterm-verify
            (macOS) or the XDG cache directory. This moves the lock; the gate's
            own tests give each throwaway repo its own, so a fixture gate never
            queues behind the gate that is running it.

A skip is an honest \"tool absent\", never a silent pass: skips are counted and
NAMED, and any run that skipped a stage or narrowed its scope is refused the
merge-contract verdict.

A stage child that never exits is a FAILURE, not a hang: every child runs under
a {CEILING} wall-clock ceiling and is killed and reported past it, because a
gate that hangs has decided nothing and says nothing. --stage-timeout <seconds>
moves that ceiling; --stage-timeout off removes it and restores the unbounded
wait.

exit 0  nothing NEW failed: everything that ran was green, or every red was
        inherited from main's receipt (the verdict says which)
exit 1  a gate FAILED — a real finding about the tree (against main: a NEW red)
exit 2  usage error
exit 3  COULD NOT RUN — the environment is broken; nothing was decided
";

/// A `--disk-floor` value: a whole number of GiB, nothing else — an empty or
/// malformed floor is a usage error, never read as zero.
fn parse_gib(v: &str) -> Result<u64, ParseError> {
    v.parse::<u64>()
        .ok()
        .filter(|g| g.checked_mul(crate::disk::GIB).is_some())
        .ok_or(ParseError::DiskFloorNeedsGib)
}

/// The `--help` text as a reader sees it: [`USAGE_TEMPLATE`] with the child
/// ceiling and the disk preflight's numbers read from the constants that
/// enforce them.
#[must_use]
pub fn usage() -> String {
    let disk = crate::disk::Budget::MEASURED;
    USAGE_TEMPLATE
        .replace(
            "{CEILING}",
            &ceiling_text(crate::exec::DEFAULT_CHILD_CEILING),
        )
        .replace("{DISK_COLD_NEED}", &crate::disk::gib(disk.need(0)))
        .replace("{DISK_LANE_CAP}", &crate::disk::gib(disk.lane_cap))
        .replace("{TEST_JOBS}", &crate::testrun::DEFAULT_JOBS.to_string())
}

/// A whole-unit English rendering of the child ceiling ("3-hour", "90-minute"),
/// so the help text names whatever [`crate::exec::DEFAULT_CHILD_CEILING`] is.
fn ceiling_text(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs.is_multiple_of(3600) {
        format!("{}-hour", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{}-minute", secs / 60)
    } else {
        format!("{secs}-second")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(args: &[&str]) -> Args {
        parse(args.iter().copied()).expect("parses")
    }

    /// `--disk-floor` takes a whole number of GiB in both spellings, and a
    /// missing, empty or malformed value is a usage error — never read as 0,
    /// which would silently switch the preflight off.
    #[test]
    fn the_disk_floor_is_whole_gib_or_a_usage_error() {
        assert_eq!(ok(&[]).disk_floor_gib, None, "not given keeps the default");
        assert_eq!(ok(&["--disk-floor", "0"]).disk_floor_gib, Some(0));
        assert_eq!(ok(&["--disk-floor=25"]).disk_floor_gib, Some(25));
        for bad in [
            vec!["--disk-floor"],
            vec!["--disk-floor", ""],
            vec!["--disk-floor="],
            vec!["--disk-floor", "40G"],
            vec!["--disk-floor", "-1"],
            vec!["--disk-floor", "1.5"],
            vec!["--disk-floor", "99999999999999999999"],
        ] {
            assert_eq!(
                parse(bad.iter().copied()),
                Err(ParseError::DiskFloorNeedsGib),
                "{bad:?}"
            );
        }
    }

    /// The value flags fail closed: a missing or malformed value is a usage error,
    /// never a default. `verify.sh` appends `--root <dir>`, so a trailing `--log`
    /// must not take `--root` as its path.
    #[test]
    fn the_value_flags_fail_closed() {
        let a = ok(&[
            "--stage-timeout",
            "120",
            "--test-threads=4",
            "--snapshot",
            "/s",
            "--log",
            "/l.log",
            "--machine-lock-dir=/m",
        ]);
        assert_eq!(a.stage_timeout, Some(Some(Duration::from_secs(120))));
        assert_eq!(a.test_threads, NonZeroU32::new(4));
        assert_eq!(a.snapshot, Some(PathBuf::from("/s")));
        assert_eq!(a.log, Some(PathBuf::from("/l.log")));
        assert_eq!(a.machine_lock_dir, Some(PathBuf::from("/m")));
        assert_eq!(ok(&["--stage-timeout=off"]).stage_timeout, Some(None));
        for (bad, err) in [
            (
                vec!["--stage-timeout", "45m"],
                ParseError::StageTimeoutNeedsSeconds,
            ),
            (
                vec!["--stage-timeout=-1"],
                ParseError::StageTimeoutNeedsSeconds,
            ),
            (
                vec!["--test-threads", "0"],
                ParseError::TestThreadsNeedsCount,
            ),
            (vec!["--test-threads=x"], ParseError::TestThreadsNeedsCount),
            (
                vec!["--snapshot"],
                ParseError::NeedsPath("--snapshot".into()),
            ),
            (
                vec!["--log", "--root", "/repo"],
                ParseError::NeedsPath("--log".into()),
            ),
            (
                vec!["--machine-lock-dir", "--fast"],
                ParseError::NeedsPath("--machine-lock-dir".into()),
            ),
            (vec!["--log", "/l", "--no-log"], ParseError::LogAndNoLog),
        ] {
            assert_eq!(parse(bad.iter().copied()), Err(err), "{bad:?}");
        }
    }

    #[test]
    fn combinations_compose_and_the_last_mode_wins() {
        let a = ok(&["--fast", "--scope", "aterm-grid"]);
        assert_eq!(a.mode, Mode::Fast);
        assert_eq!(a.scope.as_deref(), Some("aterm-grid"));
        assert_eq!(ok(&["--full", "--fast"]).mode, Mode::Fast);
        assert_eq!(ok(&["--fast", "--full"]).mode, Mode::Full);
        assert_eq!(ok(&["--full", "--measure"]).mode, Mode::Measure);
        assert_eq!(ok(&["--measure", "--fast"]).mode, Mode::Fast);
    }

    /// THE TIERS, BY MODE (2026-09-26): the default runs the LAND tier — the
    /// merge contract — and not the MEASURE tier; `--measure` runs the MEASURE
    /// tier and nothing of the contract; `--full` runs both and its own three
    /// stages. A mode that ran neither tier would run nothing, and one that
    /// ran the MEASURE tier by default would put the machine's load back into
    /// every push's verdict.
    #[test]
    fn each_mode_runs_exactly_its_tiers() {
        use crate::plan::Tier;
        let runs = |m: Mode| [Tier::Land, Tier::Measure, Tier::Full].map(|t| m.runs(t));
        assert_eq!(runs(Mode::Fast), [true, false, false]);
        assert_eq!(runs(Mode::Measure), [false, true, false]);
        assert_eq!(runs(Mode::Full), [true, true, true]);
        assert_eq!(Mode::Measure.as_str(), "measure");
        let text = usage();
        assert!(
            text.contains("tools/verify.sh --measure         # the MEASURE tier"),
            "{text}"
        );
        assert!(
            text.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains("`MEASURE tier: not part of the merge contract`"),
            "the help says the default names what it left out: {text}"
        );
        // A baseline is main's reds for the merge contract; `--measure` runs
        // none of it, so the two together are a usage error, not a quiet run.
        assert_eq!(
            parse(["--measure", "--baseline"]),
            Err(ParseError::BaselineMeasure)
        );
        assert_eq!(
            parse(["--baseline", "--full", "--measure"]),
            Err(ParseError::BaselineMeasure),
            "the last mode wins, and it is --measure"
        );
        assert!(ok(&["--measure", "--full", "--baseline"]).baseline);
        assert!(
            ParseError::BaselineMeasure
                .message()
                .contains("--measure runs the MEASURE tier alone")
        );
    }

    #[test]
    fn a_scope_without_a_crate_name_is_a_usage_error() {
        assert_eq!(parse(["--scope"]), Err(ParseError::ScopeNeedsCrateName));
        assert_eq!(parse(["--scope", ""]), Err(ParseError::ScopeNeedsCrateName));
        // The bash form that silently widened the claim to the whole workspace.
        assert_eq!(parse(["--scope="]), Err(ParseError::ScopeNeedsCrateName));
        assert_eq!(
            ParseError::ScopeNeedsCrateName.message(),
            "verify: --scope needs a crate name"
        );
    }

    #[test]
    fn an_unknown_argument_is_a_usage_error_naming_itself() {
        assert_eq!(parse(["--fest"]), Err(ParseError::Unknown("--fest".into())));
        assert_eq!(
            parse(["aterm-grid"]),
            Err(ParseError::Unknown("aterm-grid".into()))
        );
        assert_eq!(
            ParseError::Unknown("--fest".into()).message(),
            "verify: unknown argument: --fest"
        );
        // The retired modes (2026-09-27) are refused, never quietly accepted.
        for gone in ["--selftest", "--in-place", "--timings"] {
            assert_eq!(parse([gone]), Err(ParseError::Unknown(gone.into())));
        }
    }

    #[test]
    fn the_change_scoped_tier_has_its_own_flag_and_its_own_base() {
        assert!(ok(&["--changed"]).changed);
        assert_eq!(ok(&["--changed"]).base, None, "not given is not defaulted");
        assert_eq!(
            ok(&["--changed", "--base", "origin/main"]).base.as_deref(),
            Some("origin/main")
        );
        assert_eq!(
            ok(&["--changed", "--base=HEAD~5"]).base.as_deref(),
            Some("HEAD~5")
        );
        // …and it composes with the mode, which is the whole use for it.
        let a = ok(&["--full", "--changed"]);
        assert_eq!((a.mode, a.changed), (Mode::Full, true));
    }

    /// `--test-jobs` (2026-09-26): both spellings, a positive count or a usage
    /// error naming the flag, and the help names the default the constant sets.
    #[test]
    fn test_jobs_is_a_positive_count_and_the_help_names_its_default() {
        assert_eq!(ok(&["--test-jobs", "3"]).test_jobs, NonZeroU32::new(3));
        assert_eq!(ok(&["--test-jobs=1"]).test_jobs, NonZeroU32::new(1));
        assert_eq!(ok(&[]).test_jobs, None, "not given keeps the default");
        for bad in [
            vec!["--test-jobs"],
            vec!["--test-jobs=0"],
            vec!["--test-jobs", "two"],
        ] {
            assert_eq!(
                parse(bad.clone()),
                Err(ParseError::TestJobsNeedsCount),
                "{bad:?}"
            );
        }
        let text = usage();
        assert!(!text.contains("{TEST_JOBS}"), "{text}");
        assert!(
            text.contains(&format!(
                "(default\n            {})",
                crate::testrun::DEFAULT_JOBS
            )),
            "{text}"
        );
    }

    #[test]
    fn the_base_defaults_to_main_and_only_the_flag_moves_it() {
        let given = ok(&["--changed", "--base", "HEAD~1"]);
        assert_eq!(given.base_ref(), "HEAD~1");
        assert_eq!(ok(&["--changed"]).base_ref(), "main");
    }

    #[test]
    fn a_baseline_is_the_whole_tree_or_a_usage_error() {
        assert!(!ok(&[]).baseline, "a run is no baseline unless asked");
        assert!(ok(&["--baseline"]).baseline);
        let full = ok(&["--full", "--baseline"]);
        assert!(
            full.baseline && full.mode == Mode::Full,
            "either tier may baseline"
        );
        assert!(ok(&["--baseline", "--skip-gui-smoke"]).baseline);
        for narrowed in [
            vec!["--baseline", "--scope", "aterm-grid"],
            vec!["--scope=aterm-grid", "--baseline"],
            vec!["--baseline", "--changed"],
            vec!["--changed", "--base", "main", "--baseline"],
        ] {
            assert_eq!(
                parse(narrowed.iter().copied()),
                Err(ParseError::BaselineNarrowed),
                "{narrowed:?}"
            );
        }
        assert!(
            ParseError::BaselineNarrowed
                .message()
                .contains("--baseline")
        );
        assert!(usage().contains("--baseline"), "the help names it");
        assert!(
            usage().contains("N new, M\n            inherited (red on main since <sha>)"),
            "the help says what a judged verdict prints"
        );
    }

    #[test]
    fn two_narrowings_at_once_is_a_usage_error_not_a_silent_winner() {
        // Letting one win would make the printed scope a lie about what was built.
        assert_eq!(
            parse(["--scope", "aterm-grid", "--changed"]),
            Err(ParseError::ScopeAndChanged)
        );
        assert_eq!(
            parse(["--changed", "--scope=aterm-grid"]),
            Err(ParseError::ScopeAndChanged)
        );
        assert_eq!(
            ParseError::ScopeAndChanged.message(),
            "verify: --scope and --changed both narrow the build; pick one"
        );
    }

    #[test]
    fn a_base_without_changed_narrows_nothing_and_says_so() {
        assert_eq!(
            parse(["--base", "origin/main"]),
            Err(ParseError::BaseWithoutChanged)
        );
        // Including the value that happens to be the default: the flag was
        // given, and a flag that means nothing here must not look accepted.
        assert_eq!(
            parse(["--base", "main"]),
            Err(ParseError::BaseWithoutChanged)
        );
        assert_eq!(
            ParseError::BaseWithoutChanged.message(),
            "verify: --base <ref> only means something with --changed"
        );
    }

    #[test]
    fn a_base_without_a_ref_is_a_usage_error() {
        assert_eq!(
            parse(["--changed", "--base"]),
            Err(ParseError::BaseNeedsGitRef)
        );
        assert_eq!(
            parse(["--changed", "--base", ""]),
            Err(ParseError::BaseNeedsGitRef)
        );
        // The bash form that silently widened the run instead of complaining.
        assert_eq!(
            parse(["--changed", "--base="]),
            Err(ParseError::BaseNeedsGitRef)
        );
        assert_eq!(
            ParseError::BaseNeedsGitRef.message(),
            "verify: --base needs a git ref"
        );
    }

    #[test]
    fn root_is_shim_plumbing_only() {
        assert_eq!(
            ok(&["--root", "/tmp/x"]).root,
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(ok(&["--root=/tmp/x"]).root, Some(PathBuf::from("/tmp/x")));
        assert_eq!(parse(["--root"]), Err(ParseError::RootNeedsPath));
        // and it changes nothing else about the run
        let a = ok(&["--root", "/tmp/x"]);
        assert_eq!((a.mode, a.scope, a.changed), (Mode::Fast, None, false));
    }

    #[test]
    fn usage_documents_every_frozen_flag_and_every_exit_code() {
        let text = usage();
        for flag in [
            "--fast",
            "--measure",
            "--full",
            "--scope <crate>",
            "--changed",
            "--base",
        ] {
            assert!(text.contains(flag), "usage must document {flag}");
        }
        for code in ["exit 0", "exit 1", "exit 2", "exit 3"] {
            assert!(text.contains(code), "usage must document {code}");
        }
    }

    /// The merge contract is spelled BARE (2026-09-23): the usage names
    /// `tools/verify.sh` with no flag as the gate, never prescribes the
    /// no-op `--fast`, and a bare run parses to the gate's own mode.
    #[test]
    fn the_gate_is_the_bare_command() {
        let text = usage();
        assert!(
            text.contains("tools/verify.sh                   # the per-commit gate"),
            "the bare command is the gate: {text}"
        );
        assert!(
            !text.contains("verify.sh --fast"),
            "no invocation prescribes the no-op --fast: {text}"
        );
        assert_eq!(ok(&[]), ok(&["--fast"]), "--fast changes nothing");
    }
}
