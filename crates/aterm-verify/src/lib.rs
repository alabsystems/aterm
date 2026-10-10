// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-verify` — aterm's merge gate: the stage ladder, its scheduling and its
//! verdict.
//!
//! aterm has no CI and no git hooks (owner, 2026-07-06 and 2026-09-25;
//! docs/PROCESS.md): the merge contract is this gate passing locally before a
//! change lands on main. `tools/verify.sh` is the shim that
//! starts it: it resolves `$TRUST_STAGE2_BIN` to a physical path, puts it first
//! on PATH, builds this crate with it and `exec`s the binary with the caller's
//! arguments plus `--root <repo root>`. When that build fails the shim prints the
//! flag-spelling-skew diagnostic and exits `3` itself; otherwise it passes the
//! exit code through ([`exit`]).
//!
//! What the gate holds:
//!  * The `ok`/`FAIL`/`NOT RUN`/`skip` ladder vocabulary; a skip names an absent tool and is
//!    never a silent pass. Fail closed: no missing tool produces a pass.
//!  * The verdict discipline ([`verdict`]): skips are counted and named, and a
//!    narrowed run (`--scope`, `--changed`) never prints the merge-contract
//!    sentence.
//!  * `--changed [--base <ref>]` ([`changed`]) fails wide: anything the selection
//!    cannot answer (no targo, no merge-base, an unreadable graph, a
//!    manifest-level change) widens the run to the whole workspace and says so.
//!    The selection is a pure function of (diff paths, manifests, members,
//!    inverted graph), unit-tested without a repo.
//!  * Every driver invocation names its lane with `--unverified`; tippy keeps its
//!    own `CARGO_TARGET_DIR` and `TRUST_NO_MIGRATE_WARN`; the doc-running stages
//!    bind `RUSTDOC` per `stages::doc_driver`.
//!  * Independent stages run concurrently ([`sched`]), constrained by the
//!    resource each contends for ([`plan::Lane`]), while output stays in declared
//!    order. Clock-sensitive stages ([`stages::MEASURING_TESTS`], the smokes)
//!    run exclusively.
//!  * Two tiers ([`plan::Tier`]): LAND, the merge contract; and MEASURE
//!    ([`plan::MEASURE_TIER`], `--measure`/`--full`), whose receipt records
//!    `measured yes|no` ([`verdict::measured`]) for the release cutter.
//!  * `--scope=` and `--base=` with an empty value are usage errors.
//!  * Every run verifies a pinned snapshot of the caller's git checkout
//!    ([`snapshot`]). A compiler or source tree that moves mid-run stops every
//!    unstarted stage and adds a `source identity` COULD NOT RUN row
//!    ([`identity`]); a stage that already failed keeps FAIL.

pub mod build_config;
pub mod changed;
pub mod checkers;
pub mod cli;
pub mod differential;
pub mod disk;
pub mod exec;
/// aterm-digest's FIPS 180-4 SHA-256, compiled in by path as the toolchain
/// resolution is (this crate takes no dependencies); [`sha256`] is its face here.
#[path = "../../aterm-digest/src/sha256.rs"]
mod fips_sha256;
pub mod first_exec;
pub mod glob;
pub mod identity;
pub mod ladder;
pub mod lease;
pub mod libtest;
pub mod plan;
pub mod receipt;
pub mod sched;
pub mod scope;

/// SHA-256 — the ONE in the tree (`crates/aterm-digest/src/sha256.rs`), so the key a
/// MEASURE receipt is filed under and the digest the release cutter keys the bytes it
/// signs by are one function.
pub mod sha256 {
    pub use crate::fips_sha256::Sha256;

    /// The digest of `bytes`, lowercase hex.
    #[must_use]
    pub fn hex(bytes: &[u8]) -> String {
        lower_hex(&Sha256::digest(bytes))
    }

    /// The digest of the file at `path`, read in 1 MiB pieces, lowercase hex.
    ///
    /// # Errors
    /// The file cannot be opened or read.
    pub fn file_hex(path: &std::path::Path) -> std::io::Result<String> {
        use std::io::Read as _;
        let mut file = std::fs::File::open(path)?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                return Ok(lower_hex(&h.finalize()));
            }
            h.update(&buf[..n]);
        }
    }

    fn lower_hex(digest: &[u8]) -> String {
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}
// The smokes drive a unix control socket with unix process plumbing; off unix
// the two smoke rows are named skips (`stages::run_stage`).
#[cfg(unix)]
pub mod smoke;
#[cfg(unix)]
pub mod smoke_stages;
pub mod snapshot;
pub mod stages;
pub mod testrun;
/// THE toolchain resolution, shared with atpkg (`aterm help rust`) as one
/// std-only source file; this crate takes no dependencies.
#[path = "../../atpkg/src/toolchain.rs"]
pub mod toolchain;
pub mod verdict;

use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use cli::Mode;
pub use ladder::{Outcome, Report, Severity};
pub use scope::Scope;
pub use toolchain::{Toolchain, is_executable_file};
pub use verdict::{MERGE_CONTRACT_SENTENCE, Verdict};

/// Exit codes. FAILED and COULD-NOT-RUN stay apart: a broken environment is not
/// a finding about the tree. `tools/verify.sh` passes them through and returns
/// `3` itself when the gate cannot be built.
pub mod exit {
    /// Nothing new failed: everything that ran was green, or every red was
    /// inherited from main's receipt (the verdict text says *which* green).
    pub const PASS: i32 = 0;
    /// A gate FAILED — a real finding about the tree.
    pub const FAILED: i32 = 1;
    /// Usage error.
    pub const USAGE: i32 = 2;
    /// COULD NOT RUN — nothing was decided; each row says why.
    /// Never mistakable for green, and never mistakable for a finding either.
    pub const COULD_NOT_RUN: i32 = 3;
}

/// The environment, read once at startup on the main thread, so every concurrent
/// stage decides from a value a test can construct.
#[derive(Clone, Debug, Default)]
pub struct EnvSnapshot {
    pub path: OsString,
    pub home: PathBuf,
    pub trust_stage2_bin: Option<PathBuf>,
    /// `XDG_CONFIG_HOME`: where atpkg's config (`aterm/aterm.toml`, the
    /// `[packages].prefix` override) lives ([`toolchain::atpkg_prefix`]).
    pub xdg_config_home: Option<PathBuf>,
    /// `RUSTUP_HOME`, raw — where rustup keeps its toolchains when set (and not
    /// empty); [`toolchain::rustup_home`] turns it and `home` into the directory
    /// discovery probes, `~/.rustup` when unset.
    pub rustup_home: Option<OsString>,
    pub ssh_connection: Option<String>,
    /// `CARGO_BUILD_JOBS`, raw. The main and lint lanes inherit it unless the
    /// memory cap is lower ([`Ctx::with_memory_build_jobs`]); for the side
    /// lanes, whose caps are constants ([`stages::lane_build_jobs`]), it is a
    /// ceiling ([`Ctx::build_jobs_ceiling`]).
    pub cargo_build_jobs: Option<OsString>,
    /// `RUSTDOC` or `CARGO_BUILD_RUSTDOC`: the caller's doc-driver override,
    /// which cargo prefers over the config's `[build] rustdoc`. The doc-driver
    /// rule honours it and never turns it into a COULD-NOT-RUN.
    pub rustdoc_override: Option<OsString>,
}

impl EnvSnapshot {
    /// Read the process environment.
    #[must_use]
    pub fn capture() -> Self {
        let var_path = |k: &str| std::env::var_os(k).map(PathBuf::from);
        Self {
            path: std::env::var_os("PATH").unwrap_or_default(),
            home: var_path("HOME").unwrap_or_default(),
            trust_stage2_bin: var_path("TRUST_STAGE2_BIN"),
            xdg_config_home: var_path("XDG_CONFIG_HOME").filter(|p| !p.as_os_str().is_empty()),
            rustup_home: std::env::var_os("RUSTUP_HOME"),
            ssh_connection: std::env::var("SSH_CONNECTION").ok(),
            cargo_build_jobs: std::env::var_os("CARGO_BUILD_JOBS"),
            // No empty-filter: cargo treats a set-but-empty RUSTDOC as set (only
            // RUSTC_WRAPPER is treated as unset when empty), so it still masks
            // CARGO_BUILD_RUSTDOC; a broken export fails as "(caller's RUSTDOC)".
            rustdoc_override: std::env::var_os("RUSTDOC")
                .or_else(|| std::env::var_os("CARGO_BUILD_RUSTDOC")),
        }
    }
}

/// THE BYTES MEASURE JUDGES (2026-10-09, build once): a release bundle the
/// release built once and hands the MEASURE tier (`--measure <APP>`, `--full
/// <APP>`), never a release `aterm` the gate builds itself. Its executable's
/// SHA-256 is the key the MEASURE receipt is filed under
/// ([`receipt::measure_key`]), so `measured yes` is a fact about these bytes, and
/// the release signs only bytes whose digest holds one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureArtifact {
    /// The bundle, canonical: `<…>/aterm.app`.
    pub app: PathBuf,
    /// SHA-256 of `<app>/Contents/MacOS/aterm`, lowercase hex.
    pub macho_sha256: String,
}

impl MeasureArtifact {
    /// The bundle at `app`: a directory holding an executable
    /// `Contents/MacOS/aterm`, digested now.
    ///
    /// # Errors
    /// What `app` is instead, in words: no such bundle, no executable `aterm` in
    /// it, or an executable that cannot be read.
    pub fn at(app: &Path) -> Result<Self, String> {
        let app = std::fs::canonicalize(app)
            .map_err(|e| format!("{} is not a release bundle: {e}", app.display()))?;
        let aterm = app.join("Contents/MacOS/aterm");
        if !is_executable_file(&aterm) {
            return Err(format!(
                "{} is not a release bundle: it holds no executable Contents/MacOS/aterm",
                app.display()
            ));
        }
        let macho_sha256 =
            sha256::file_hex(&aterm).map_err(|e| format!("read {}: {e}", aterm.display()))?;
        Ok(Self { app, macho_sha256 })
    }

    /// `<app>/Contents/MacOS/<name>`: `aterm`, or one of the argv0 names the
    /// bundle links to it (`aterm-gui`, `aterm-ctl`).
    #[must_use]
    pub fn bin(&self, name: &str) -> PathBuf {
        self.app.join("Contents/MacOS").join(name)
    }
}

/// Everything a stage needs, immutable for the whole run so stages can share it
/// across threads without a lock.
#[derive(Debug)]
pub struct Ctx {
    pub root: PathBuf,
    pub mode: Mode,
    pub scope: Scope,
    pub tools: Toolchain,
    /// PATH for every child: the resolved stage2 directory first when it holds a
    /// `targo`. Passed per child, since `set_var` would race the stage threads.
    pub path_env: OsString,
    /// Private scratch directory for captured child output, removed at the end.
    pub scratch: PathBuf,
    pub env: EnvSnapshot,
    /// Stages decided before the ladder was planned: `--changed`'s selection,
    /// which produces the [`Scope`] [`plan::plan`] builds from. Printed first
    /// and tallied like any other stage.
    pub prelude: Vec<Report>,
    /// The caller's checkout this run's root is a snapshot of
    /// ([`Ctx::in_snapshot_of`]). `None` only for in-process test contexts,
    /// whose ladder runs on a root they made.
    pub snapshot_of: Option<PathBuf>,
    /// `verify: …` header lines — the snapshot's (cold or pruned lanes) and
    /// the pinned child facts — printed under the source line.
    pub notes: Vec<String>,
    /// Variables removed from every child's inherited environment
    /// ([`exec::ExecEnv::remove_env`]): [`CHILD_ENV_REMOVED`], always.
    pub child_env_remove: Vec<&'static str>,
    /// Variables given to every child ([`exec::ExecEnv::add_env`]): [`CHILD_ENV`],
    /// seeded at construction, plus the facts [`Ctx::with_pinned_child_facts`]
    /// appends. Builders append; assigning would drop the seeded
    /// `CARGO_INCREMENTAL=0`.
    pub child_env_add: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// The wall-clock ceiling on one stage child: [`exec::DEFAULT_CHILD_CEILING`]
    /// unless `--stage-timeout` moved it, `None` for `--stage-timeout off`.
    pub child_ceiling: Option<std::time::Duration>,
    /// `--no-windows`: every stage that puts a window on the desktop
    /// ([`plan::WINDOWED`]) answers a named SKIP.
    pub no_windows: bool,
    /// The release bundle the MEASURE tier judges (`--measure <APP>`, `--full
    /// <APP>`): the paint and spin suites are handed its `aterm`, the pacing
    /// smoke drives its `aterm-gui` and `aterm-ctl`, and the run's MEASURE
    /// receipt is filed under its executable's digest. `None` for a run of the
    /// LAND tier alone; a MEASURE stage without one COULD NOT RUN.
    pub measure_artifact: Option<MeasureArtifact>,
    /// The source state a snapshot's sync verified. The tripwire arms on it
    /// (and compares it with a fresh capture) rather than re-arming from
    /// whatever the root holds by the time the ladder starts.
    pub source_baseline: Option<identity::TreeState>,
    /// The disk preflight's budget ([`disk`]): [`disk::Budget::MEASURED`] in a
    /// real run; the preflight tests scale it down to bytes.
    pub disk_budget: disk::Budget,
    /// `--disk-floor`: require exactly this much free and skip the estimate.
    /// Fixture tests set `Some(0)` so they never depend on the host volume.
    pub disk_floor: Option<u64>,
    /// Reads free space for the preflight in place of [`disk::read_free`]'s
    /// `df`; `None` in a real run. A function of the root, so a test can answer
    /// from what the root holds at call time.
    pub disk_free: Option<fn(&Path) -> disk::Reading>,
    /// Where each stage's finish line goes the moment it finishes
    /// ([`Ctx::with_progress_log`]), ahead of the ladder's declared-order print.
    pub progress_log: Option<std::fs::File>,
    /// `--baseline`: this run records main's own reds — HEAD must be a clean
    /// commit of `origin/main` — and publishes its receipt for branches to be
    /// judged against ([`differential`]).
    pub baseline: bool,
    /// `--test-jobs`: the most test binaries the test stage runs at once
    /// ([`testrun::Limits::by_load`]; [`testrun::DEFAULT_JOBS`] unless the flag
    /// says otherwise).
    pub test_jobs: u32,
    /// The `RUST_TEST_THREADS` [`Ctx::with_pinned_child_facts`] pinned, which
    /// the test stage hands every binary it runs, as cargo's own run does. `None`
    /// until it pinned one ([`Ctx::test_threads`] then answers the machine's
    /// parallelism, as the pin would).
    pub test_threads: Option<u32>,
    /// The program cargo runs in place of each test binary to record it
    /// ([`testrun::RECORD_FLAG`]): this very binary, unless a test names the
    /// built `aterm-verify` because it runs the gate in-process.
    pub test_recorder: Option<PathBuf>,
    /// Reads what the test pool widens by ([`testrun::Limits::by_load`]) in
    /// place of [`Ctx::test_load`]'s machine reading;
    /// `None` in a real run. A fixture ladder names an idle or a busy machine
    /// with it, so how the test stage wires the load into its pool is a test
    /// of its own, whatever the host's load.
    pub test_load: Option<fn() -> Option<f64>>,
    /// How the headless smoke paces its typing burst:
    /// [`smoke_stages::Pacing::PRODUCTION`] except in the gate's fixture ladders
    /// ([`Ctx::with_smoke_pacing`]).
    #[cfg(unix)]
    pub smoke_pacing: smoke_stages::Pacing,
    /// The stages a FIXTURE ladder runs ([`Ctx::with_only_stages`]); every other
    /// planned stage answers one SKIP row, so the run cannot claim the merge
    /// contract, yet it still files a strength-1 whole-tree receipt that can
    /// serve as a base. `None` in a real run: only [`Ctx::new_with_tools`] makes
    /// a `Ctx` and it says `None`, and nothing in `main.rs` or `cli.rs` sets it.
    pub only_stages: Option<Vec<plan::StageId>>,
    /// The host's physical memory in bytes ([`exec::host_memory_bytes`]),
    /// which sizes the land tier's builds at once ([`plan::land_builds`]) and
    /// the test pool ([`testrun::memory_jobs`]). `None` — unread, and in every
    /// `Ctx` a test makes unless it says otherwise — takes the floor pool.
    pub host_memory: Option<u64>,
    /// `host_memory` is Linux's `MemTotal`, which reads under the installed
    /// RAM, so a RAM figure derived from it says so.
    pub host_memtotal: bool,
    /// The memory the host can give builds, in bytes
    /// ([`exec::host_usable_bytes`]: physical less wired on macOS, Linux's
    /// `MemAvailable`). Wired memory past what a healthy host holds comes off
    /// the RAM every memory decision uses ([`Ctx::build_memory`]). `None` —
    /// unread, and in every `Ctx` a test makes unless it says otherwise —
    /// leaves the physical figure to decide alone.
    pub host_usable: Option<u64>,
    /// The compiler jobs the host's memory holds ([`memory_jobs_held`]), set by
    /// [`Ctx::with_memory_build_jobs`] on every host whose wired memory is past
    /// the allowance. Every lane that compiles stays at or under it
    /// ([`Ctx::build_jobs_ceiling`]); a figure above a lane's own count changes
    /// nothing. `None` leaves the count to the caller's `CARGO_BUILD_JOBS`, or
    /// to cargo.
    pub memory_jobs: Option<u32>,
}

/// A `CARGO_BUILD_JOBS` value as a job count: a positive integer, surrounding
/// whitespace allowed. Anything else is `None`, which no job decision reads as
/// a count; the raw value still reaches the main lane by inheritance, where
/// cargo judges it.
#[must_use]
pub fn parse_build_jobs(raw: Option<&OsStr>) -> Option<u32> {
    raw.and_then(OsStr::to_str)
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| n > 0)
}

/// The compiler jobs a host's memory holds: one per
/// [`plan::WORKSPACE_BUILD_BYTES`] of `usable`, at least one, on a host whose
/// wired memory is past the allowance ([`plan::build_memory`] below `ram`).
/// `None` on a host within the allowance, or when either figure is unread.
///
/// It is every lane's ceiling ([`Ctx::build_jobs_ceiling`]), so it only ever
/// lowers a count: a figure above a lane's own (a 36 GiB Mac with 4.2 GiB
/// wired holds 15) changes nothing.
#[must_use]
pub fn memory_jobs_held(ram: Option<u64>, usable: Option<u64>) -> Option<u32> {
    let (ram, usable) = (ram?, usable?);
    if plan::build_memory(Some(ram), Some(usable)) >= Some(ram) {
        return None;
    }
    Some(u32::try_from((usable / plan::WORKSPACE_BUILD_BYTES).max(1)).unwrap_or(u32::MAX))
}

/// The `CARGO_BUILD_JOBS` the lanes that inherit the environment get from the
/// memory: [`memory_jobs_held`], when that is fewer than such a build would
/// otherwise run; `None` leaves their count alone.
///
/// What a build would otherwise run is the caller's `CARGO_BUILD_JOBS`
/// (`caller`, [`parse_build_jobs`]) or cargo's default, one per core
/// (`cores`). So with no caller value, a host whose usable memory holds as
/// many jobs as it has cores or more (a 36 GiB Mac with 4.2 GiB wired holds 15
/// on 14 cores; a 2-core host with 4.5 GiB usable holds 2) gets no export, and
/// a caller's export at or below the figure stands. The side lanes, whose own
/// counts can exceed the cores, take [`memory_jobs_held`] as their ceiling
/// whatever this answers.
#[must_use]
pub fn memory_job_cap(
    ram: Option<u64>,
    usable: Option<u64>,
    cores: u32,
    caller: Option<u32>,
) -> Option<u32> {
    let otherwise = caller.unwrap_or(cores).max(1);
    memory_jobs_held(ram, usable).filter(|&held| held < otherwise)
}

/// The git stamp [`Ctx::with_pinned_child_facts`] hands every child: the commit,
/// full commit and dev counter `crates/aterm/build.rs` reads instead of
/// asking git. `aterm-update-core`'s `env_reads` gate lists this table as one
/// the gate writes.
pub const GIT_STAMP_ENV: [&str; 3] = [
    "ATERM_BUILD_GIT_COMMIT",
    "ATERM_BUILD_GIT_COMMIT_FULL",
    "ATERM_BUILD_DEV_COMMITS",
];

/// Variables removed from every child's inherited environment.
///
/// `CARGO_TARGET_DIR`: every lane names its own directory under the run's root
/// ([`plan::lane_dir`]); a caller's redirect would build somewhere other than
/// where the stages look for binaries.
///
/// [`LIBTEST_UNSTABLE_ENV`]: the test run passes `-Zunstable-options
/// --report-time` ([`stages::LIBTEST_REPORT_TIME`]), under which libtest also
/// reads `RUST_TEST_SHUFFLE*` (reorders tests) and `RUST_TEST_TIME_*` (a
/// malformed value panics the binary). Removing them keeps the caller's shell
/// out of the merge contract.
pub const CHILD_ENV_REMOVED: [&str; 6] = [
    "CARGO_TARGET_DIR",
    "RUST_TEST_SHUFFLE",
    "RUST_TEST_SHUFFLE_SEED",
    "RUST_TEST_TIME_UNIT",
    "RUST_TEST_TIME_INTEGRATION",
    "RUST_TEST_TIME_DOCTEST",
];

/// What libtest reads from its environment once `-Zunstable-options` is on
/// ([`stages::LIBTEST_REPORT_TIME`]) — each one in [`CHILD_ENV_REMOVED`].
pub const LIBTEST_UNSTABLE_ENV: [&str; 5] = [
    "RUST_TEST_SHUFFLE",
    "RUST_TEST_SHUFFLE_SEED",
    "RUST_TEST_TIME_UNIT",
    "RUST_TEST_TIME_INTEGRATION",
    "RUST_TEST_TIME_DOCTEST",
];

/// Variables set in every child's environment: after [`CHILD_ENV_REMOVED`] and
/// before a stage's own [`exec::Cmd::envs`], so the gate beats the caller's shell
/// and a stage's own setting beats the gate ([`exec::ExecEnv::add_env`]). They
/// seed [`Ctx::child_env_add`] at construction, so every context has them.
///
/// `CARGO_INCREMENTAL=0`: the gate builds each commit once and reuses no
/// incremental cache, whose artifacts would otherwise dominate the lanes' disk
/// use ([`disk`]). A caller's `CARGO_INCREMENTAL=1` is overridden on purpose.
pub const CHILD_ENV: [(&str, &str); 1] = [("CARGO_INCREMENTAL", "0")];

/// The run's toolchain, discovered once from `env` under the pin `root` declares
/// and the atpkg prefix atpkg itself would read.
///
/// `main` calls this once and hands the result to the lane stamp, `--changed`'s
/// selection and every stage ([`Ctx::new_with_tools`]). Separate discoveries can
/// disagree: the staleness rule's `trustc -vV` probes have a 5 s bound, and a
/// probe that times out never demotes.
#[must_use]
pub fn run_toolchain(env: &EnvSnapshot, root: &Path) -> Toolchain {
    let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    Toolchain::discover_with_store(
        env.trust_stage2_bin.as_deref(),
        &toolchain::rustup_home(env.rustup_home.as_deref(), &env.home),
        Some(&prefix),
        &env.path,
        crate::toolchain::pinned_channel(root).as_deref(),
    )
}

impl Ctx {
    /// Build the run context, discovering its toolchain ([`run_toolchain`]).
    /// `scratch` must already exist. A caller that already has the run's
    /// toolchain uses [`Self::new_with_tools`]: two discoveries can disagree.
    #[must_use]
    pub fn new(
        root: PathBuf,
        mode: Mode,
        scope: Scope,
        env: EnvSnapshot,
        scratch: PathBuf,
    ) -> Self {
        let tools = run_toolchain(&env, &root);
        Self::new_with_tools(root, mode, scope, env, scratch, tools)
    }

    /// [`Self::new`] with the run's toolchain already discovered
    /// ([`run_toolchain`]), so the lane stamp, the scope and every stage agree.
    #[must_use]
    pub fn new_with_tools(
        root: PathBuf,
        mode: Mode,
        scope: Scope,
        env: EnvSnapshot,
        scratch: PathBuf,
        tools: Toolchain,
    ) -> Self {
        let path_env = tools.path_with_stage2_first(&env.path);
        Self {
            root,
            mode,
            scope,
            tools,
            path_env,
            scratch,
            env,
            prelude: Vec::new(),
            snapshot_of: None,
            notes: Vec::new(),
            child_env_remove: CHILD_ENV_REMOVED.to_vec(),
            child_env_add: CHILD_ENV
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            child_ceiling: Some(exec::DEFAULT_CHILD_CEILING),
            no_windows: false,
            measure_artifact: None,
            source_baseline: None,
            disk_budget: disk::Budget::MEASURED,
            disk_floor: None,
            disk_free: None,
            progress_log: None,
            baseline: false,
            test_jobs: testrun::DEFAULT_JOBS,
            test_threads: None,
            test_recorder: None,
            test_load: None,
            #[cfg(unix)]
            smoke_pacing: smoke_stages::Pacing::PRODUCTION,
            only_stages: None,
            host_memory: None,
            host_memtotal: false,
            host_usable: None,
            memory_jobs: None,
        }
    }

    /// The RAM every memory decision of the plan uses
    /// ([`plan::build_memory`] over [`Ctx::host_memory`] and
    /// [`Ctx::host_usable`]).
    #[must_use]
    pub fn build_memory(&self) -> Option<u64> {
        plan::build_memory(self.host_memory, self.host_usable)
    }

    /// The cores a load average is read against: the machine's parallelism.
    #[must_use]
    pub fn cores(&self) -> u32 {
        std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX))
    }

    /// The most compiler jobs any build of the run may take: the smaller of the
    /// caller's `CARGO_BUILD_JOBS` ([`parse_build_jobs`]) and the jobs the
    /// memory holds ([`Ctx::memory_jobs`]); `None` leaves cargo's own count.
    /// The side lanes take it as the ceiling of their constants
    /// ([`stages::lane_jobs`]). The main and lint lanes inherit the caller's
    /// value, or get the memory's through [`Ctx::child_env_add`] when it is
    /// below what they would otherwise run ([`memory_job_cap`]), so they never
    /// run more than this either.
    #[must_use]
    pub fn build_jobs_ceiling(&self) -> Option<u32> {
        let caller = parse_build_jobs(self.env.cargo_build_jobs.as_deref());
        caller.into_iter().chain(self.memory_jobs).min()
    }

    /// The most test binaries that run at once ([`Ctx::test_jobs`], at least one).
    #[must_use]
    pub fn test_jobs(&self) -> usize {
        usize::try_from(self.test_jobs.max(1)).unwrap_or(1)
    }

    /// The `RUST_TEST_THREADS` the run pinned, or the machine's parallelism.
    #[must_use]
    pub fn test_threads(&self) -> u32 {
        self.test_threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX))
        })
    }

    /// What the test pool reads against the cores: [`Ctx::test_load`]'s answer,
    /// else the cores busy over the last second ([`exec::busy_cores`]), else the
    /// one-minute load ([`exec::load_average`]).
    #[must_use]
    pub fn test_load(&self) -> Option<f64> {
        self.test_load.map_or_else(
            || exec::busy_cores().or_else(exec::load_average),
            |read| read(),
        )
    }

    /// The recording runner ([`Ctx::test_recorder`]): the named program, else
    /// this process's own executable.
    #[must_use]
    pub fn test_recorder(&self) -> Option<PathBuf> {
        self.test_recorder
            .clone()
            .or_else(|| std::env::current_exe().ok())
    }

    /// `--test-jobs <n>` ([`Ctx::test_jobs`]).
    #[must_use]
    pub fn with_test_jobs(mut self, jobs: Option<std::num::NonZeroU32>) -> Self {
        if let Some(n) = jobs {
            self.test_jobs = n.get();
        }
        self
    }

    /// A `--baseline` run ([`Ctx::baseline`]).
    #[must_use]
    pub fn with_baseline(mut self, baseline: bool) -> Self {
        self.baseline = baseline;
        self
    }

    /// The checkout the caller ran the gate from: the snapshot's source, or
    /// the root itself. Receipts and the base are read and written there.
    #[must_use]
    pub fn caller_root(&self) -> PathBuf {
        self.snapshot_of
            .clone()
            .unwrap_or_else(|| self.root.clone())
    }

    /// This run's root is a snapshot of `caller`. `tree` is the state the
    /// sync verified; the run's tripwire arms on it.
    #[must_use]
    pub fn in_snapshot_of(
        mut self,
        caller: PathBuf,
        tree: identity::TreeState,
        notes: Vec<String>,
    ) -> Self {
        self.snapshot_of = Some(caller);
        self.source_baseline = Some(tree);
        self.notes.extend(notes);
        self
    }

    /// Pin what every child must agree on, once, on the main thread:
    ///
    /// * The git stamp ([`GIT_STAMP_ENV`]), all three facts or none. Otherwise
    ///   `crates/aterm/build.rs` asks git and `rerun-if-changed`s files in the
    ///   common git dir, which any worktree's `fetch` or `gc` touches mid-run.
    /// * Test concurrency: `RUST_TEST_THREADS` is `test_threads`
    ///   (`--test-threads`) or the machine's parallelism, never the caller's
    ///   shell's value. Named in the notes.
    /// * The toolchain: `$TRUST_STAGE2_BIN` is the resolved stage2 directory, so
    ///   the xtask verbs that run their own `Toolchain::discover` (`gate lint
    ///   --fmt-only`, `gate forge`, `gate cells-foreign`) use this run's compiler
    ///   rather than a rustup link or the store's moving `current`. Set only when
    ///   a `targo` was found there.
    #[must_use]
    pub fn with_pinned_child_facts(mut self, test_threads: Option<std::num::NonZeroU32>) -> Self {
        // Not said as a note: the toolchain header already names the directory.
        if self.tools.have_targo() {
            self.child_env_add.push((
                "TRUST_STAGE2_BIN".into(),
                self.tools.stage2_dir.clone().into_os_string(),
            ));
        }
        let git = |args: &[&str]| -> Option<String> {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.root)
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        let commit = git(&["rev-parse", "--short=12", "HEAD"]).filter(|c| !c.is_empty());
        let full_commit = git(&["rev-parse", "HEAD"]).filter(|c| !c.is_empty());
        let dev_commits = git(&["describe", "--tags", "--match", "v*.*.0", "--abbrev=0"])
            .and_then(|tag| git(&["rev-list", &format!("{tag}..HEAD"), "--count"]))
            .filter(|c| !c.is_empty());
        // All or none: the build script pins only with every fact, so half a pin
        // would leave its watch armed while looking pinned.
        if let (Some(commit), Some(full_commit), Some(dev)) = (commit, full_commit, dev_commits) {
            let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
            let stamp = if dirty {
                format!("{commit}-dirty")
            } else {
                commit
            };
            self.notes.push(format!(
                "child env: git stamp pinned to {stamp} (+{dev} since tag)"
            ));
            let [commit_key, full_key, dev_key] = GIT_STAMP_ENV;
            self.child_env_add.push((commit_key.into(), stamp.into()));
            self.child_env_add
                .push((full_key.into(), full_commit.into()));
            self.child_env_add.push((dev_key.into(), dev.into()));
        } else {
            self.notes.push(
                "child env: git stamp NOT pinned (this root answers no commit) — the build \
                 script falls back to probing git, and its watch stays armed"
                    .to_string(),
            );
        }

        let threads = test_threads.map_or_else(
            || {
                std::thread::available_parallelism()
                    .map_or(1, |n| n.get())
                    .to_string()
            },
            |n| n.to_string(),
        );
        self.notes
            .push(format!("child env: RUST_TEST_THREADS pinned to {threads}"));
        self.test_threads = threads.parse().ok();
        self.child_env_add
            .push(("RUST_TEST_THREADS".into(), threads.into()));
        self
    }

    /// Extra `verify: …` header lines.
    #[must_use]
    pub fn with_notes(mut self, notes: impl IntoIterator<Item = String>) -> Self {
        self.notes.extend(notes);
        self
    }

    /// Move the stage-child ceiling (`--stage-timeout`; `None` is `off`).
    #[must_use]
    pub fn with_child_ceiling(mut self, ceiling: Option<std::time::Duration>) -> Self {
        self.child_ceiling = ceiling;
        self
    }

    /// `--no-windows`: every stage that puts a window on the desktop
    /// ([`plan::WINDOWED`]) answers a named SKIP.
    #[must_use]
    pub fn with_no_windows(mut self, no_windows: bool) -> Self {
        self.no_windows = no_windows;
        self
    }

    /// The release bundle the MEASURE tier judges ([`Ctx::measure_artifact`]),
    /// named on the run's header with the digest its receipt is filed under.
    #[must_use]
    pub fn with_measure_artifact(mut self, artifact: Option<MeasureArtifact>) -> Self {
        if let Some(a) = &artifact {
            self.notes.push(format!(
                "verify: MEASURE judges {} (Contents/MacOS/aterm sha256 {}) — the bytes a \
                 release signs; this run builds no release aterm",
                a.app.display(),
                a.macho_sha256
            ));
        }
        self.measure_artifact = artifact;
        self
    }

    /// The host's physical memory, in bytes ([`Ctx::host_memory`]).
    #[must_use]
    pub fn with_host_memory(mut self, bytes: Option<u64>) -> Self {
        self.host_memory = bytes;
        self
    }

    /// The memory the host can give builds, in bytes ([`Ctx::host_usable`]).
    #[must_use]
    pub fn with_host_usable(mut self, bytes: Option<u64>) -> Self {
        self.host_usable = bytes;
        self
    }

    /// On a host whose wired memory is past the allowance ([`plan::build_memory`]
    /// below the physical RAM), in every mode, cap each build's compiler jobs at
    /// one per [`plan::WORKSPACE_BUILD_BYTES`] of usable memory, at least one
    /// ([`memory_jobs_held`]): as every side lane's ceiling ([`Ctx::memory_jobs`],
    /// [`Ctx::build_jobs_ceiling`]), and by `CARGO_BUILD_JOBS` in every child's
    /// environment for the lanes that inherit it when that is fewer than they
    /// would otherwise run ([`memory_job_cap`]). Measured 2026-10-06: with
    /// 4.5 GiB usable, the serialised test compile alone still ran six compilers
    /// at once and swapped (every large crate in state U, 4.6 of 5 GiB of swap),
    /// so serialising the builds is not enough without it. A host with room
    /// keeps cargo's own job count, and a caller's smaller `CARGO_BUILD_JOBS`
    /// stands. Call after the memory figures are set.
    #[must_use]
    pub fn with_memory_build_jobs(self) -> Self {
        let cores = self.cores();
        self.with_memory_build_jobs_on(cores)
    }

    /// [`Ctx::with_memory_build_jobs`] on a host with `cores` cores.
    fn with_memory_build_jobs_on(mut self, cores: u32) -> Self {
        // EVERY MODE, NOT ONLY THE LAND TIER (2026-10-06): a `--measure` run has
        // no land-tier build to serialise, but its release build (the gate's own
        // until 2026-10-09) still ran cargo's six compilers on 4.4 GiB, and its
        // measuring tests still compile. The trigger is the degraded host
        // itself — wired memory past the allowance, the figure every other
        // memory decision reads ([`plan::build_memory`]) — not one plan's shape.
        // The side lanes name their own `CARGO_BUILD_JOBS`, which beats
        // `child_env_add` ([`exec::ExecEnv::add_env`]), so the cap reaches them
        // through `memory_jobs` and [`Ctx::build_jobs_ceiling`] — recorded on
        // EVERY degraded host (review of 2026-10-07): their constants (driver 8)
        // exceed a small host's cores, so a 2-core host with 4.5 GiB usable,
        // which needs no export, still ran 8 driver compilers.
        let held = memory_jobs_held(self.host_memory, self.host_usable);
        self.memory_jobs = held;
        let caller = parse_build_jobs(self.env.cargo_build_jobs.as_deref());
        if let Some(jobs) = memory_job_cap(self.host_memory, self.host_usable, cores, caller) {
            self.notes.push(format!(
                "builds: at most {jobs} compiler(s) at once (memory)"
            ));
            self.child_env_add
                .push(("CARGO_BUILD_JOBS".into(), jobs.to_string().into()));
        } else if let Some(held) = held {
            // Said only where the figure lowers a side lane: below the widest
            // lane's own count and below the caller's export, which already
            // caps every lane ([`Ctx::build_jobs_ceiling`]). A host whose memory
            // holds more than any lane runs says nothing, as the other memory
            // notes say only what they change.
            let widest = [
                plan::Lane::DriverTarget,
                plan::Lane::XtaskTarget,
                plan::Lane::TrustTarget,
            ]
            .into_iter()
            .filter_map(stages::lane_build_jobs)
            .max()
            .unwrap_or(0);
            if held < widest && caller.is_none_or(|caller| held < caller) {
                self.notes.push(format!(
                    "builds: at most {held} compiler(s) at once (memory)"
                ));
            }
        }
        self
    }

    /// Whether [`Ctx::host_memory`] is Linux's `MemTotal`
    /// ([`Ctx::host_memtotal`]).
    #[must_use]
    pub fn with_host_memtotal(mut self, memtotal: bool) -> Self {
        self.host_memtotal = memtotal;
        self
    }

    /// Attach a stage decided before the plan existed (see [`Ctx::prelude`]).
    #[must_use]
    pub fn with_prelude(mut self, report: Option<Report>) -> Self {
        self.prelude.extend(report);
        self
    }

    /// Write every stage's finish line to `log` as it happens
    /// ([`Ctx::progress_log`]). The handle should append: the ladder's own
    /// copy goes to the same file, and an appending write lands whole.
    #[must_use]
    pub fn with_progress_log(mut self, log: Option<std::fs::File>) -> Self {
        self.progress_log = log;
        self
    }

    /// Replace the disk preflight's estimate with a fixed requirement
    /// ([`Ctx::disk_floor`]): `--disk-floor <GiB>`
    /// ([`cli::Args::disk_floor_gib`]), and the gate's own fixture tests with
    /// `0`. The `verify: disk …` line says when a floor is in force.
    #[must_use]
    pub fn with_disk_floor(mut self, bytes: u64) -> Self {
        self.disk_floor = Some(bytes);
        self
    }

    /// Move the smokes' pacing ([`Ctx::smoke_pacing`]). Nothing but the gate's
    /// fixture tests calls this: a real run always paces as measured.
    #[cfg(unix)]
    #[must_use]
    pub fn with_smoke_pacing(mut self, pacing: smoke_stages::Pacing) -> Self {
        self.smoke_pacing = pacing;
        self
    }

    /// Run only `stages` ([`Ctx::only_stages`]); every other planned stage is
    /// one named SKIP row that spawns nothing, so the run still ends on the one
    /// verdict, receipt and tripwire path a real run takes, over a ladder as
    /// short as the law under test needs.
    ///
    /// WHY IT CANNOT BUY A CLAIM. The merge-contract sentence needs a ladder
    /// with nothing skipped ([`verdict::claims`]), and a filtered-out stage IS a
    /// skip — so a run that left any stage out can neither print the sentence
    /// nor file `merge-contract yes`, whichever stages it kept. Naming every
    /// planned stage is the unfiltered run.
    ///
    /// WHAT IT STILL FILES. Only the claim is gated: the run's receipt is
    /// written as any whole-tree run's, `merge-contract no` with its skips
    /// listed, so it is strength 1 ([`receipt::strength`]) and
    /// [`differential::usable`] takes it as a base, which is what the
    /// base-layer fixture tests read (`--baseline` publishes it as main's
    /// reds). That is why only a fixture may call this: nothing in `main.rs`
    /// or `cli.rs` does, and a real run through it would file a base that
    /// lists only the stages it kept. The callers are the gate's fixture
    /// tests, which gate the tripwire, receipt and base layers, whose laws are
    /// decided around the ladder and not by any one stage of it (measured
    /// 2026-10-02 at load 17: a scoped fixture ladder took 6 s, of which its
    /// two test stages were 0.3 s).
    #[must_use]
    pub fn with_only_stages(mut self, stages: &[plan::StageId]) -> Self {
        self.only_stages = Some(stages.to_vec());
        self
    }

    /// Budget the disk preflight with other numbers ([`Ctx::disk_budget`]).
    #[must_use]
    pub fn with_disk_budget(mut self, budget: disk::Budget) -> Self {
        self.disk_budget = budget;
        self
    }

    /// Give the disk preflight this free-space reader instead of `df`
    /// ([`Ctx::disk_free`]).
    #[must_use]
    pub fn with_disk_free(mut self, read: fn(&Path) -> disk::Reading) -> Self {
        self.disk_free = Some(read);
        self
    }

    /// The command environment: cwd is the repo root (stages pass root-relative
    /// paths), PATH is the computed one, and every child gets the wall-clock
    /// ceiling so a hung stage ends the run red.
    #[must_use]
    pub fn exec_env(&self) -> exec::ExecEnv<'_> {
        exec::ExecEnv {
            cwd: &self.root,
            path: &self.path_env,
            scratch: &self.scratch,
            child_ceiling: self.child_ceiling,
            remove_env: &self.child_env_remove,
            add_env: &self.child_env_add,
        }
    }

    /// `tools/` — where the guard scripts live.
    #[must_use]
    pub fn tools_dir(&self) -> PathBuf {
        self.root.join("tools")
    }
}

/// The line naming the compiler every stage runs, from the directory the
/// toolchain walk settled on and whether a `targo` was found there. A relative
/// path is called out rather than printed as an answer.
#[must_use]
pub fn toolchain_header_line(
    stage2_dir: &std::path::Path,
    channel: Option<&str>,
    have_targo: bool,
) -> String {
    let channel = channel.unwrap_or("<unpinned>");
    if !have_targo {
        return format!(
            "verify: toolchain NONE — channel \"{channel}\" resolved to no usable targo \
             (looked last at {})\n",
            stage2_dir.display()
        );
    }
    if !stage2_dir.is_absolute() {
        return format!(
            "verify: toolchain {} — RELATIVE, so this run cannot say which compiler it \
             used (channel \"{channel}\")\n",
            stage2_dir.display()
        );
    }
    format!(
        "verify: toolchain {} (channel \"{channel}\", resolved once for this run)\n",
        stage2_dir.display()
    )
}

/// The line under the header explaining why the rustup `trust` toolchain is not
/// the one named ([`toolchain::Demoted`]): it is older than the atpkg store's.
#[must_use]
pub fn toolchain_demoted_line(demoted: &toolchain::Demoted) -> String {
    format!("verify: toolchain note — {}\n", demoted.sentence())
}

/// [`toolchain_header_line`] for a live run, plus [`toolchain_demoted_line`] when the
/// walk demoted a rustup entry to get there.
#[must_use]
pub fn toolchain_header(ctx: &Ctx) -> String {
    let mut out = toolchain_header_line(
        &ctx.tools.stage2_dir,
        crate::toolchain::pinned_channel(&ctx.root).as_deref(),
        ctx.tools.have_targo(),
    );
    if let Some(d) = &ctx.tools.demoted {
        out.push_str(&toolchain_demoted_line(d));
    }
    out
}

/// Run the whole gate: ladder and verdict. Returns the process exit code.
///
/// `out` receives, in order: the [`toolchain_header`], any lease line, the
/// prelude rungs, a narrowed run's [`narrowing_note`], the `verify:` header
/// lines (source, [`checkers`], notes, the [`differential`] base, build dirs over
/// the cap, disk), then the ladder in declared order with a `  time  ` line under
/// each stage — or, in its place, a COULD NOT RUN row for an unreadable source,
/// a refused `--baseline` or a failed [`disk`] preflight. After the ladder: a
/// `source identity` row if anything moved mid-run, the MEASURE-tier line, the
/// verdict (or the gate-defect `FAIL` and `VERIFY: COULD NOT RUN` lines), and a
/// `--baseline` run's publish line. Live progress goes to stderr; every stage's
/// [`finish_line`] goes to [`Ctx::progress_log`] in the order stages finish.
///
/// # Errors
/// Propagates write failures on `out`.
pub fn run(ctx: &Ctx, out: &mut dyn Write) -> std::io::Result<i32> {
    // Name the compiler before anything is decided.
    out.write_all(toolchain_header(ctx).as_bytes())?;
    // Lease the compiler ([`lease`]) for the whole ladder, so atpkg's gc and
    // updates leave the pinned build alone between stages. Said in one line when
    // held or refused; a lease that cannot be taken never stops the run.
    let _lease = if ctx.tools.have_targo() {
        let prefix = toolchain::atpkg_prefix(&ctx.env.home, ctx.env.xdg_config_home.as_deref());
        let who = format!(
            "aterm-verify (pid {}) \u{2014} {} in {}",
            std::process::id(),
            if ctx.mode.runs(plan::Tier::Land) {
                "the merge contract"
            } else {
                "the MEASURE tier"
            },
            ctx.root.display()
        );
        let taken = lease::take(&prefix, &ctx.tools.stage2_dir, &who, lease::WAIT);
        out.write_all(taken.header_line().as_bytes())?;
        Some(taken)
    } else {
        None
    };
    // The change-scope stage first: it is what chose the scope every header
    // below prints, so a reader meets the narrowing before its consequences.
    for r in &ctx.prelude {
        out.write_all(r.render().as_bytes())?;
    }
    // A narrowed run says so before it spends an hour, not only in its verdict.
    if let Some(note) = narrowing_note(&ctx.scope) {
        writeln!(out, "{note}")?;
    }

    // WHAT THIS RUN IS VERIFYING, captured before anything is planned and
    // re-checked while it runs (2026-09-13). The 14 h run this answers was
    // pulled four times mid-ladder and printed one verdict over all of them.
    // The toolchain half carries the spec checkers the tests will run
    // (2026-09-26, [`checkers`]): resolved once, here, named on the ladder
    // and in the receipt, and re-resolved with the compiler before every stage.
    let mut toolchain = identity::ToolchainIdentity::of(&ctx.tools, &ctx.path_env, &ctx.scratch);
    toolchain.checkers = Some(checkers::Checkers::capture(&ctx.env.home, &ctx.path_env));
    let tripwire = identity::Tripwire::arm_against(
        &ctx.root,
        &ctx.path_env,
        ctx.source_baseline.clone(),
        toolchain,
    );
    if let Some(line) =
        tripwire.header_line(&snapshot::place(&ctx.root, ctx.snapshot_of.as_deref()))
    {
        out.write_all(line.as_bytes())?;
    }
    if let Some(c) = &tripwire.toolchain.checkers {
        out.write_all(c.header_line().as_bytes())?;
    }
    for note in &ctx.notes {
        writeln!(out, "{note}")?;
    }
    // Say before an hour is spent how wide the test pool is
    // ([`plan::test_pool_note`]) and, on a host whose wired memory leaves too
    // little for the land tier's builds at once, that they build one or two
    // at a time ([`plan::land_builds`]); the RAM is stated once, before them.
    // Then, apart from memory, that the trust lane waits for the land tier's
    // builds, test run and doctests, and why ([`plan::trust_overlap`]: the
    // verifier's budgets are clocks).
    let stages = plan::plan(ctx);
    for line in plan::memory_notes(
        ctx.host_memory,
        ctx.host_usable,
        ctx.host_memtotal,
        &stages,
        ctx.test_jobs(),
    ) {
        writeln!(out, "{line}")?;
    }
    if let Some(line) = plan::trust_overlap(&stages).note() {
        writeln!(out, "{line}")?;
    }

    // A git checkout the gate cannot read has no tripwire, so no stage runs.
    if let identity::SourceIdentity::Unreadable(why) = &tripwire.source {
        let mut r = Report::new("source identity");
        r.cannot_run(identity::unreadable_label(why));
        out.write_all(r.render().as_bytes())?;
        let mut reports = ctx.prelude.clone();
        reports.push(r);
        let verdict = verdict::verdict(ctx.mode, &ctx.scope, &ladder::tally(&reports));
        out.write_all(verdict.text.as_bytes())?;
        out.flush()?;
        return Ok(verdict.exit);
    }

    // Settle what this run is judged against ([`differential`]) before spending
    // anything: main's receipt for the base, or the absolute rule. A `--baseline`
    // that cannot be one (dirty tree, HEAD not main's) is refused here and leaves
    // no receipt.
    let caller = ctx.caller_root();
    let head = match &tripwire.source {
        identity::SourceIdentity::Git(tree) => Some(tree),
        _ => None,
    };
    if ctx.baseline {
        let refusal = match head {
            Some(tree) => {
                differential::baseline_refusal(&caller, &tree.head, !tree.dirty.is_empty())
            }
            None => Some("this root is not a git checkout, so it has no commit to baseline".into()),
        };
        if let Some(why) = refusal {
            let mut r = Report::new("baseline preflight");
            r.cannot_run(format!("--baseline refused: {why}"));
            out.write_all(r.render().as_bytes())?;
            let mut reports = ctx.prelude.clone();
            reports.push(r);
            let verdict = verdict::verdict(ctx.mode, &ctx.scope, &ladder::tally(&reports));
            out.write_all(verdict.text.as_bytes())?;
            out.flush()?;
            return Ok(verdict.exit);
        }
    }
    // A base serves only a run made by the same tools: this run's, as its
    // receipt will name them.
    let tools = run_tools(ctx, &tripwire);
    // A `--measure` run is judged by the absolute rule: the MEASURE tier is
    // not part of the merge contract, and a release cut takes its receipt only
    // when nothing in it was red — so no red of it is ever inherited.
    let base_plan = head.map(|tree| {
        if ctx.mode.runs(plan::Tier::Land) {
            differential::resolve(&caller, &tree.head, ctx.baseline, &tools)
        } else {
            differential::Plan::Absolute {
                why: MEASURE_IS_ABSOLUTE.to_string(),
                chain: None,
            }
        }
    });
    if let (Some(plan), Some(tree)) = (&base_plan, head) {
        out.write_all(plan.header_line(ctx.baseline, &tree.head).as_bytes())?;
    }

    // Disk preflight ([`disk`]): budget what this run will write from what its
    // lanes hold, remove lanes over the cap first, and refuse (COULD NOT RUN, no
    // receipt) when the volume has less free, printing the arithmetic.
    let lanes = disk::measure_lanes(&ctx.root);
    let mut plan = disk::plan(
        ctx.disk_budget,
        ctx.disk_floor,
        &lanes,
        disk::Owner::Snapshot,
    );
    if plan.remove {
        plan.unremoved = snapshot::remove_lanes(&ctx.root);
        for why in &plan.unremoved {
            writeln!(out, "verify: build dirs over the cap: {why}")?;
        }
    }
    let reading = ctx
        .disk_free
        .map_or_else(|| disk::read_free(&ctx.root), |read| read(&ctx.root));
    out.write_all(disk::header_line(&reading, &plan, &ctx.root).as_bytes())?;
    if let Err(why) = disk::decide(&reading, &plan, &ctx.root) {
        let mut r = Report::new("disk preflight");
        r.cannot_run(why);
        // After a removal the remedy sizes what is left, not what was.
        let lanes = if plan.remove {
            disk::measure_lanes(&ctx.root)
        } else {
            lanes
        };
        r.raw(disk::remedy(
            &ctx.root,
            &plan,
            &lanes,
            &reading,
            ctx.snapshot_of.as_deref(),
        ));
        out.write_all(r.render().as_bytes())?;
        let mut reports = ctx.prelude.clone();
        reports.push(r);
        let verdict = verdict::verdict(ctx.mode, &ctx.scope, &ladder::tally(&reports));
        out.write_all(verdict.text.as_bytes())?;
        out.flush()?;
        return Ok(verdict.exit);
    }

    let mut reports: Vec<Report> = ctx.prelude.clone();
    reports.reserve(stages.len());
    let mut err = None;

    // Per-stage (start offset, running time) for the `  time  ` line, on stdout
    // so the captured log shows where the time went. `decisions` readers skip it.
    let t0 = Instant::now();
    type Clock = (Duration, Duration, Option<ladder::StageLoad>);
    let clocks: Mutex<Vec<Option<Clock>>> = Mutex::new(vec![None; stages.len()]);
    // The one-minute load at each stage's start and end ([`ladder::StageLoad`]),
    // on its `time` line, in the receipt and under the verdict's timing label.
    let cores = ctx.cores();

    // Stages run concurrently, so a waiting stage would otherwise be silent. The
    // START line is stderr- and terminal-only; the FINISH line carries the outcome
    // word (`outcome_word`) and goes to the run's log as the stage finishes.
    // stdout stays the ordered ladder.
    let progress = std::io::stderr().is_terminal();
    let done = sched::run_stages(
        &stages,
        |spec| {
            let started = Instant::now();
            let begun = t0.elapsed();
            let load_start = exec::load_average();
            if progress {
                eprintln!("verify: start  {}", spec.title);
            }
            // A stage that would build or drive something must not start on a
            // tree or a compiler other than the one this run named. Its row
            // names the stage; why is said once, by the `source identity` row
            // the trip (it latches) adds after the ladder.
            let left_out = ctx
                .only_stages
                .as_ref()
                .is_some_and(|only| !only.contains(&spec.id));
            let report = if left_out {
                // A fixture ladder's left-out stage: no child, no tripwire read.
                // It is a SKIP row, which is what keeps the run from claiming
                // the contract ([`Ctx::with_only_stages`]).
                let mut r = Report::new(spec.title.clone());
                r.skip(format!(
                    "{} (not in this fixture ladder's stages)",
                    spec.title
                ));
                r
            } else if spec.lane != plan::Lane::Pure
                && tripwire.check(identity::CHECK_CACHE).is_some()
            {
                let mut r = Report::new(spec.title.clone());
                r.cannot_run(format!(
                    "the tree or the toolchain moved before `{}` started",
                    spec.title
                ));
                r
            } else {
                stages::run_stage(ctx, spec)
            };
            // A left-out stage did nothing a cached check could have missed.
            if !left_out {
                tripwire.stage_finished();
            }
            let ran = started.elapsed();
            let load_end = exec::load_average();
            let mut report = report;
            report.load = ladder::StageLoad::of(load_start, load_end, cores);
            let finished = finish_line(&spec.title, outcome_word(&report), begun, ran);
            if progress {
                eprint!("{finished}");
            }
            if let Some(mut log) = ctx.progress_log.as_ref() {
                // A record, never a decision: a log that cannot be written costs
                // the line, not the run.
                let _ = log.write_all(finished.as_bytes());
            }
            if let Some(i) = stages.iter().position(|s| std::ptr::eq(s, spec))
                && let Ok(mut c) = clocks.lock()
            {
                c[i] = Some((begun, ran, report.load));
            }
            report
        },
        |i, report| {
            // Flush per stage so a watcher sees each rung as it is decided.
            let clock = clocks.lock().ok().and_then(|c| c.get(i).copied().flatten());
            let mut text = report.render();
            if let Some((begun, ran, load)) = clock {
                text.push_str(&time_line(begun, ran, load));
            }
            if err.is_none()
                && let Err(e) = out.write_all(text.as_bytes()).and_then(|()| out.flush())
            {
                err = Some(e);
            }
        },
    );
    if let Some(e) = err {
        return Err(e);
    }
    reports.extend(done);

    // The check a stage start may have cached is never cached here: a tree
    // that moved during the LAST stage is as unverified as one that moved
    // during the first.
    let moved = tripwire.check(Duration::ZERO);
    if let Some(why) = &moved {
        let mut r = Report::new("source identity");
        r.cannot_run(identity::tripped_label(why));
        out.write_all(r.render().as_bytes())?;
        reports.push(r);
    }
    // Did this run measure the tree: asked of the MEASURE tier's own stages, not
    // the verdict (a skipped cross-cell check still measured; a red paint row did not,
    // whatever main's receipt excuses).
    let stage_reports = &reports[ctx.prelude.len()..ctx.prelude.len() + stages.len()];
    let measured = ctx.mode.runs(plan::Tier::Measure).then(|| {
        // The bytes the receipt is filed under must still be the bytes judged.
        artifact_unmoved(ctx.measure_artifact.as_ref())
            .and_then(|()| verdict::measured(&stages, stage_reports, &ctx.scope, moved.is_some()))
    });

    // A planned stage that recorded no outcome is invisible to `tally`, so a
    // whole-tree run missing it could still print the merge-contract sentence.
    // That is a gate defect, not a finding: exit COULD-NOT-RUN.
    if let Some(silent) = reports.iter().find(|r| r.outcomes().next().is_none()) {
        writeln!(
            out,
            "\n  FAIL  gate defect: stage `{}` was planned but recorded no outcome — \
             it cannot be counted, so this run cannot claim anything.",
            silent.title
        )?;
        writeln!(
            out,
            "  VERIFY: COULD NOT RUN — a stage decided nothing. This is NOT a finding \
             about your change."
        )?;
        out.flush()?;
        return Ok(exit::COULD_NOT_RUN);
    }

    let tally = ladder::tally(&reports);
    let now = epoch_secs();
    let against = base_plan
        .as_ref()
        .map_or_else(differential::Against::absolute, |p| p.against(now));
    let verdict = verdict::verdict_against(ctx.mode, &ctx.scope, &tally, &against);
    let since_from = base_plan.as_ref().and_then(|p| p.since_from(now));
    let judged = Judged {
        tools: &tools,
        against: &against,
        since_from: since_from.as_ref(),
        now,
        moved: moved.is_some(),
        measured: measured.as_ref().map(Result::is_ok),
        loads: stage_reports
            .iter()
            .filter_map(|r| r.load.map(|l| (l, r.title.clone())))
            .collect(),
    };
    // The verdict is written before the receipt is filed: a verdict that cannot
    // be written leaves no receipt, and once it is out nothing after it changes
    // the exit code.
    if let Some(m) = &measured {
        out.write_all(verdict::measured_line(m).as_bytes())?;
    }
    out.write_all(verdict.text.as_bytes())?;
    out.flush()?;
    let standing = write_receipt(ctx, &tripwire, &verdict, &tally, &judged);
    if ctx.baseline
        && let (Some(text), Some(tree)) = (standing, head)
    {
        let _ = out
            .write_all(publish_line(&caller, &tree.head, &text, ctx).as_bytes())
            .and_then(|()| out.flush());
    }
    Ok(verdict.exit)
}

/// The compiler, spec checkers and build environment this run used, as its
/// receipt names them: `<stage2 bin dir> trustc <commit-hash>` (`unknown` when
/// trustc named none), the checkers' summary, the inherited build and test-run
/// configuration ([`differential::build_env`]) and the cargo config files its
/// children read ([`build_config::of_this_process`]) — what a base must match
/// ([`differential::tools_differ`]).
fn run_tools(ctx: &Ctx, tripwire: &identity::Tripwire) -> differential::Tools {
    differential::Tools {
        toolchain: format!(
            "{} trustc {}",
            ctx.tools.stage2_dir.display(),
            tripwire.toolchain.commit.as_deref().unwrap_or("unknown")
        ),
        checkers: tripwire
            .toolchain
            .checkers
            .as_ref()
            .map(checkers::Checkers::summary)
            .unwrap_or_default(),
        build_env: Some(differential::build_env(std::env::vars_os())),
        build_config: build_config::of_this_process(&ctx.root),
    }
}

/// Seconds since the epoch, now.
fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Why a `--measure` run has no base: the `verify: base …` line's reason.
pub const MEASURE_IS_ABSOLUTE: &str = "a --measure run has no base";

/// How a run was judged, for its receipt.
struct Judged<'a> {
    /// What ran it: the tools its base was compared with ([`run_tools`]).
    tools: &'a differential::Tools,
    against: &'a differential::Against,
    /// Where each failure's since is carried from.
    since_from: Option<&'a differential::BaseReds>,
    now: u64,
    /// The source or the compiler moved under the run (the tripwire fired):
    /// some of its findings came from other bytes or tools than the commit's,
    /// so its receipt lists none ([`write_receipt`]).
    moved: bool,
    /// Whether the run measured the tree ([`verdict::measured`]); `None` for
    /// a run that did not run the MEASURE tier.
    measured: Option<bool>,
    /// The load around each stage that could be read, with its title.
    loads: Vec<(ladder::StageLoad, String)>,
}

/// A MEASURE run's artifact is still the bytes it was handed: its executable's
/// digest at the end of the run is the digest its receipt is filed under
/// ([`MeasureArtifact::macho_sha256`]). A run handed none measured nothing.
fn artifact_unmoved(artifact: Option<&MeasureArtifact>) -> Result<(), String> {
    let Some(a) = artifact else {
        return Err("no release bundle was handed to judge (--measure <APP>)".to_string());
    };
    match sha256::file_hex(&a.bin("aterm")) {
        Ok(now) if now == a.macho_sha256 => Ok(()),
        Ok(now) => Err(format!(
            "{}'s executable changed under the run ({} -> {now})",
            a.app.display(),
            a.macho_sha256
        )),
        Err(e) => Err(format!(
            "{}'s executable could not be read again: {e}",
            a.app.display()
        )),
    }
}

/// Publish a `--baseline` run's receipt ([`differential::publish`]) and say
/// what happened, in one line. A failure costs other machines the baseline,
/// never this run its verdict: the receipt already stands in the store every
/// worktree of this repository reads.
fn publish_line(caller: &Path, head: &str, text: &str, ctx: &Ctx) -> String {
    let reds = receipt::Receipt::parse(text)
        .and_then(|r| r.failures)
        .map_or(0, |f| f.len());
    match differential::publish(caller, head, text, &ctx.scratch) {
        Ok(()) => format!(
            "verify: baseline published — {} {} now carries main's receipt for {} ({reds} \
             red(s)); a branch whose base is {} is judged against it\n",
            differential::MAIN_REMOTE,
            differential::NOTES_REF,
            differential::short(head),
            differential::short(head)
        ),
        Err(why) => format!(
            "verify: baseline NOT published ({why}) — the receipt stands in this repository's \
             store, which every worktree here reads; other machines see it only once a \
             --baseline run publishes\n"
        ),
    }
}

/// Record what this run decided about this commit and its tree ([`receipt`]),
/// where the release cutter reads it (`crates/aterm-release/src/gates.rs`
/// `receipt_report`): filed under the commit and `<commit>^{tree}`, naming the
/// scope (`changed:<base>` for a change-scoped run), the compiler and the spec
/// checkers. A MEASURE-tier run also records `measured yes|no` and the artifact
/// it judged under that artifact's measure key (`gates::measured`); a
/// `--measure` run files there only.
///
/// It itemizes every finding with when main first went red on it (from
/// `judged.since_from`), the reds the since chain held that this run could not
/// see ([`differential::hidden_failures`]), the base, and the findings judged
/// inherited ([`differential`]). A run whose source or compiler moved lists no
/// failures, so it never serves as a base. Returns the receipt text now under
/// the commit, which a `--baseline` run publishes.
///
/// Only a run with a source identity over a clean tree files one, and a weaker
/// receipt never replaces the commit's whole-tree one ([`receipt::write`]).
/// COULD NOT RUN is written as `verdict COULD-NOT-RUN`, `merge-contract no`. A
/// run that never reached its verdict writes nothing, so the last real judgement
/// stands. Failures are announced on stderr and never change the verdict.
fn write_receipt(
    ctx: &Ctx,
    tripwire: &identity::Tripwire,
    verdict: &verdict::Verdict,
    tally: &ladder::Tally,
    judged: &Judged<'_>,
) -> Option<String> {
    let identity::SourceIdentity::Git(tree) = &tripwire.source else {
        return None;
    };
    if !tree.dirty.is_empty() {
        eprintln!(
            "verify: no gate receipt — this run verified {} plus uncommitted work, which no \
             commit holds; commit, then run the gate on the commit you push",
            tree.head
        );
        return None;
    }
    let caller = ctx.caller_root();
    let tools = judged.tools.clone();
    // What the verdict excused, by id: every finding judged INHERITED.
    let (base, inherited) = match judged.against {
        differential::Against::Base(base) => (
            Some(base.commit.clone()),
            differential::inherited_ids(tally, base),
        ),
        differential::Against::Absolute(_) => (None, Vec::new()),
    };
    let r = receipt::Receipt {
        head: tree.head.clone(),
        tree: receipt::tree_of(&caller, &tree.head),
        mode: ctx.mode.as_str().to_string(),
        scope: ctx.scope.receipt_word(),
        verdict: match verdict.exit {
            exit::PASS => "PASS",
            exit::FAILED => "FAIL",
            _ => "COULD-NOT-RUN",
        }
        .to_string(),
        merge_contract: verdict.claims_merge_contract,
        measured: judged.measured,
        artifact: judged
            .measured
            .and(ctx.measure_artifact.as_ref())
            .map(|a| a.macho_sha256.clone()),
        skipped: receipt::skipped_line(&tally.skips),
        toolchain: tools.toolchain,
        checkers: tools.checkers,
        build_env: tools.build_env,
        build_config: tools.build_config,
        // No list from a run that moved: it would serve as main's reds
        // ([`differential::usable`]) with findings from other bytes or tools.
        failures: (!judged.moved).then(|| {
            differential::recorded_failures(tally, judged.since_from, &tree.head, judged.now)
        }),
        base,
        inherited,
        baseline: ctx.baseline,
        // Reds the since chain held that this run could not itemize (a test log
        // it could not account for hides that stage's), so their clocks run on.
        hidden: if judged.moved {
            Vec::new()
        } else {
            differential::hidden_failures(tally, judged.since_from)
        },
        loads: judged.loads.clone(),
        when: judged.now,
    };
    match receipt::write(&caller, &r) {
        Ok(receipt::Written::Stored(_)) => Some(r.render()),
        Ok(receipt::Written::KeptWholeTree(path)) => {
            eprintln!(
                "verify: {} keeps its stronger receipt ({}); this run ({}, scope {}) does not \
                 replace it",
                differential::short(&r.head),
                path.display(),
                r.verdict,
                r.scope
            );
            std::fs::read_to_string(&path).ok()
        }
        Err(e) => {
            eprintln!(
                "verify: cannot write the gate receipt for {} (from {}): {e} — the release \
                 cutter will count this commit as ungated",
                r.head,
                caller.display()
            );
            Some(r.render())
        }
    }
}

/// The line a narrowed run prints before any stage: why it is narrower than the
/// merge contract, and that even a green verdict will not discharge it. `None`
/// for the whole-tree run.
#[must_use]
pub fn narrowing_note(scope: &Scope) -> Option<String> {
    let why = scope.narrowing()?;
    Some(format!(
        "verify: NARROWED — {why}. Even green, this run is not the merge contract."
    ))
}

/// The `  time  ` line under a stage: its running time, its start offset from
/// the ladder (time spent waiting for its lane, an earlier exclusive stage or the
/// lanes it is ordered after; an exclusive stage waits for everything earlier),
/// and the one-minute load at its start and end when both were read
/// ([`ladder::StageLoad`]).
#[must_use]
pub fn time_line(begun: Duration, ran: Duration, load: Option<ladder::StageLoad>) -> String {
    let load = load.map_or_else(String::new, |l| format!("; load {}", l.describe()));
    format!(
        "  time  {:.1}s (started +{:.1}s{load})\n",
        ran.as_secs_f64(),
        begun.as_secs_f64()
    )
}

/// The line a stage's end writes to the run's log as it happens: its title,
/// its outcome word, how long it ran and when it started.
#[must_use]
pub fn finish_line(title: &str, word: &str, begun: Duration, ran: Duration) -> String {
    format!(
        "verify: finish {title} — {word} ({:.1}s, started +{:.1}s)\n",
        ran.as_secs_f64(),
        begun.as_secs_f64()
    )
}

/// A stage's outcome in one word, for its timing row and its finish line.
#[must_use]
pub fn outcome_word(report: &Report) -> &'static str {
    let mut word = "ok";
    for (o, _) in report.outcomes() {
        match o {
            Outcome::Fail(Severity::CouldNotRun) => return "could-not-run",
            Outcome::Fail(Severity::GateFailed) => word = "FAIL",
            Outcome::Skip if word == "ok" => word = "skip",
            _ => {}
        }
    }
    word
}

/// Where the gate keeps its own copy of each run's ladder, under
/// [`identity::GATE_STATE_DIR`]. Named here because `main` writes it and the
/// tripwire's remedy sentence points at it.
pub const LOG_DIR: &str = "logs";

/// Find the repo root by walking up from `start` to a directory holding both
/// `Cargo.toml` and `tools/verify.sh`. The shim passes `--root`, which skips this.
#[must_use]
pub fn locate_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() && d.join("tools/verify.sh").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// `mktemp -d`: creates the private directory atomically, and `/tmp` keeps
/// unix-socket paths under macOS's 104-byte `sockaddr_un` limit.
///
/// # Errors
/// Fails when `mktemp` is absent or refuses to create the directory.
pub fn mktemp_dir(tag: &str) -> std::io::Result<PathBuf> {
    let out = Command::new("mktemp")
        .arg("-d")
        .arg(format!("/tmp/{tag}.XXXXXX"))
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "mktemp -d /tmp/{tag}.XXXXXX failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        return Err(std::io::Error::other("mktemp -d printed no path"));
    }
    Ok(PathBuf::from(path))
}

/// Put an executable `#!/bin/sh` stand-in running `body` at `path`, replacing
/// whatever is there. A test helper, public only so the integration suites
/// and this crate's own unit tests share one copy: the crate takes no
/// dependencies, so it cannot borrow one from a tempfile crate.
///
/// ONE FILE PER TEXT (measured 2026-09-30, again 2026-10-02). The first-exec
/// assessment is made one file at a time machine-wide, and it is per inode,
/// not per content: 117 ms per new file at idle, so a fixture ladder's
/// thirty-odd stand-ins spent 3.5 s of its 5 s alone, and the binary's
/// fixtures, run fourteen at once, took 50 s each. On 2026-10-02 at load 60 on
/// 14 cores thirty copies of one text ran in 4.3 to 47.9 s, thirty hard links
/// of one new file in 0.54 to 0.60 s, and a fresh copy of a text already run
/// still paid 0.15 to 1.7 s. So each distinct text is written once per process and linked
/// into place, and the first fixture to run it pays for all of them. The
/// shared file is read-only, so a stand-in written over in place fails loudly
/// instead of rewriting every fixture's; replace one through here or by `mv`,
/// which both swap the link. The tripwires' stamp (device, inode, length,
/// mtime) never moves when a link is added. The shared files live in a `/tmp`
/// directory of the process's own, which the system's `/tmp` sweep removes.
///
/// WRITE THE TEXT SO IT HOLDS NO PATH OF ITS OWN, or it is shared by no one:
/// `$0` is the link that was run, so a name or a sibling file comes from it
/// (`${0##*/}`, `${0%/*}/../log`) and a per-test value from the environment
/// the test's command carries.
///
/// # Panics
/// When `path`'s directory cannot be made or the stand-in cannot be written:
/// a fixture without its stand-in decides nothing.
#[cfg(unix)]
#[doc(hidden)]
pub fn standin(path: &Path, body: &str) {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{OnceLock, PoisonError};
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    static SHARED: Mutex<BTreeMap<String, PathBuf>> = Mutex::new(BTreeMap::new());

    let text = format!("#!/bin/sh\n{body}\n");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    let _ = std::fs::remove_file(path);
    let shared = DIR
        .get_or_init(|| mktemp_dir("atv-standins").ok())
        .as_ref()
        .and_then(|dir| {
            let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(file) = shared.get(&text) {
                return Some(file.clone());
            }
            let file = dir.join(shared.len().to_string());
            std::fs::write(&file, &text).ok()?;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o555)).ok()?;
            shared.insert(text.clone(), file.clone());
            Some(file)
        });
    if shared.is_some_and(|file| std::fs::hard_link(file, path).is_ok()) {
        return;
    }
    // No shared directory, or `path` is on another filesystem: a plain copy
    // is slower to first exec and as correct.
    std::fs::write(path, &text).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// [`standin`] for a text that cannot be shared: a body that is DATA, not text.
/// Installs the one constant text `. "$0.body"` at `path` (so every stand-in
/// made here, whatever it does, is one inode and one first-exec assessment per
/// process) and writes `body` beside it as `<path>.body`, a plain file the
/// stand-in SOURCES and never execs, so it is never assessed at all.
///
/// WHY. [`standin`] shares by text, and a fixture's text is rarely shared: it
/// bakes in a path (`{root}`, `{exe}`) or a code (`exit 1` against `exit 2`),
/// so each fixture was a fresh inode. Measured 2026-10-02 at load 75: twenty
/// stand-ins of twenty distinct texts took 4.3-5.2 s to first-run (0.22 s
/// each), twenty links of one text with a sourced body each, 0.44-0.48 s: one
/// assessment (0.21 s) and 12-14 ms for each of the other nineteen.
///
/// WHAT CHANGES FOR THE BODY, against the same text run as the script
/// (measured under macOS's `/bin/sh`, bash 3.2 in POSIX mode):
/// - it is sourced into the stand-in's own shell: `$0` is still the link's
///   path (never `<path>.body`), `$@`, `$#`, `$*` are the stand-in's arguments,
///   and `exit N`, `exec` and `kill -KILL $$` act on the stand-in itself;
/// - a `return` is legal and ends the stand-in with that status, where a script
///   would report an error and carry on;
/// - a syntax error names `<path>.body` and exits 1, where a script's exits 2;
/// - the body has no shebang line of its own: it is `sh`'s, as in [`standin`];
/// - `<path>.body` sits in `path`'s directory, so a test that lists the
///   directory sees it, and the stand-in is run by a path (a stage's absolute
///   one): `.` searches `PATH` for a `$0` with no slash, and where bash falls
///   back to the working directory, dash answers `not found`.
///
/// WHAT IT IS NOT FOR. The stamp (device, inode, length, mtime) a tripwire
/// reads never moves when a body is rewritten, which is the point and the
/// limit: a stand-in whose changing identity a tripwire must see (a compiler
/// rewritten mid-run) goes through [`standin`] with the text that moves it,
/// and one the stage hard-links elsewhere (`ln -f "$0"`) carries no body with
/// it. Rewrite a body through here: the data file is replaced by `rename`, so
/// a stage that sources it as it is rewritten reads the old text or the new
/// one, never a truncated file.
///
/// # Panics
/// As [`standin`], and when the body cannot be written.
#[cfg(unix)]
#[doc(hidden)]
pub fn standin_body(path: &Path, body: &str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);

    let mut data = path.as_os_str().to_owned();
    data.push(".body");
    let data = PathBuf::from(data);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    // Beside the data file, so the rename never crosses a filesystem; named by
    // pid and a counter, so two writers of one path never share a temp file.
    let mut tmp = data.as_os_str().to_owned();
    tmp.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, format!("{body}\n")).expect("write the stand-in's body");
    std::fs::rename(&tmp, &data).expect("rename the stand-in's body into place");
    // The data is in place before the link is, so a stand-in is never run
    // without its body.
    standin(path, r#". "$0.body""#);
}

/// `command -v <name>` against the PATH the gate hands its children.
#[must_use]
pub fn have_on_path(name: &str, path: &OsStr) -> bool {
    std::env::split_paths(path).any(|d| is_executable_file(&d.join(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MEMORY CAP ONLY LOWERS (the review of 2026-10-07): it caps a host
    /// whose wired memory is past the allowance, and never above the caller's
    /// `CARGO_BUILD_JOBS` or, with none, the cores cargo would use. Until then a
    /// 36 GiB, 14-core Mac with 4.2 GiB wired handed every main-lane child 15
    /// in place of a caller's 4, and a 128 GiB host 61.
    #[test]
    fn the_memory_job_cap_only_ever_lowers_the_count() {
        const GIB: u64 = disk::GIB;
        let degraded = (Some(8 * GIB), Some(GIB * 9 / 2));
        let cap = |(ram, usable): (Option<u64>, Option<u64>), cores, caller| {
            memory_job_cap(ram, usable, cores, caller)
        };
        assert_eq!(cap(degraded, 8, None), Some(2), "4.5 GiB: two compilers");
        assert_eq!(cap(degraded, 8, Some(4)), Some(2), "below the caller's 4");
        assert_eq!(cap(degraded, 8, Some(2)), None, "the caller's 2 stands");
        assert_eq!(cap(degraded, 8, Some(1)), None, "never above the caller");
        assert_eq!(cap(degraded, 2, None), None, "never above the cores");
        assert_eq!(
            cap((Some(8 * GIB), Some(GIB)), 8, None),
            Some(1),
            "at least one"
        );
        assert_eq!(
            cap((Some(8 * GIB), Some(7 * GIB)), 8, None),
            None,
            "a healthy host keeps its count"
        );
        assert_eq!(cap((None, Some(GIB)), 8, None), None, "unread RAM");
        assert_eq!(cap((Some(8 * GIB), None), 8, None), None, "unread usable");
        // Past the allowance yet roomy: usable memory holds more jobs than the
        // count it would lower.
        let mac36 = (Some(36 * GIB), Some(36 * GIB - GIB * 42 / 10));
        assert_eq!(cap(mac36, 14, Some(4)), None, "36 GiB, caller 4");
        assert_eq!(cap(mac36, 14, None), None, "36 GiB, 14 cores");
        let gate128 = (Some(128 * GIB), Some(122 * GIB));
        assert_eq!(cap(gate128, 18, None), None, "128 GiB, 18 cores");
        // Whatever the host, a cap is at least one and below what the build
        // would otherwise run, and is the figure the side lanes take.
        for ram_gib in [4, 8, 16, 36, 128] {
            for wired_gib in 0..=ram_gib {
                let host = (Some(ram_gib * GIB), Some((ram_gib - wired_gib) * GIB));
                for cores in [1, 2, 4, 14, 18] {
                    for caller in [None, Some(1), Some(3), Some(64)] {
                        if let Some(jobs) = cap(host, cores, caller) {
                            assert!(
                                (1..caller.unwrap_or(cores)).contains(&jobs),
                                "{ram_gib} GiB, {wired_gib} wired, {cores} cores, \
                                 caller {caller:?}: {jobs}"
                            );
                            assert_eq!(Some(jobs), memory_jobs_held(host.0, host.1));
                        }
                    }
                }
            }
        }
    }

    /// The side lanes this file's tests judge, with their own job counts.
    const SIDE_LANES: [plan::Lane; 3] = [
        plan::Lane::DriverTarget,
        plan::Lane::XtaskTarget,
        plan::Lane::TrustTarget,
    ];

    /// THE SIDE LANES TAKE THE MEMORY'S FIGURE ON EVERY DEGRADED HOST (the
    /// review of 2026-10-07): their own counts (driver 8, xtask and trust 4)
    /// can exceed a small host's cores, so the figure is their ceiling
    /// even where the main lane needs no export. Until then a 2-core host with
    /// 4.5 GiB usable ran 8 driver compilers, and a 4-core one with 12 GiB
    /// usable of 16 ran 8 where the memory holds 6. The ceiling never raises a
    /// lane: a roomy degraded host keeps every constant.
    #[test]
    fn the_memory_figure_is_every_side_lanes_ceiling_on_a_degraded_host() {
        const GIB: u64 = disk::GIB;
        let lanes = |held| SIDE_LANES.map(|l| stages::lane_jobs(l, held));
        let constants = SIDE_LANES.map(stages::lane_build_jobs);
        let two_core = (Some(8 * GIB), Some(GIB * 9 / 2));
        assert_eq!(memory_job_cap(two_core.0, two_core.1, 2, None), None);
        assert_eq!(memory_jobs_held(two_core.0, two_core.1), Some(2));
        assert_eq!(lanes(Some(2)), [Some(2); 3], "2 cores, 4.5 GiB usable");
        let four_core = (Some(16 * GIB), Some(12 * GIB));
        assert_eq!(memory_job_cap(four_core.0, four_core.1, 4, None), None);
        assert_eq!(memory_jobs_held(four_core.0, four_core.1), Some(6));
        assert_eq!(
            lanes(Some(6)),
            [Some(6), Some(4), Some(4)],
            "4 cores, 12 GiB usable of 16"
        );
        // Roomy yet past the allowance: the figure is above every constant.
        let mac36 = memory_jobs_held(Some(36 * GIB), Some(36 * GIB - GIB * 42 / 10));
        let gate128 = memory_jobs_held(Some(128 * GIB), Some(122 * GIB));
        assert_eq!((mac36, gate128), (Some(15), Some(61)));
        assert_eq!(lanes(mac36), constants, "36 GiB keeps every constant");
        assert_eq!(lanes(gate128), constants, "128 GiB keeps every constant");
        // Within the allowance, or unread: no figure.
        assert_eq!(memory_jobs_held(Some(8 * GIB), Some(7 * GIB)), None);
        assert_eq!(memory_jobs_held(None, Some(GIB)), None);
        assert_eq!(memory_jobs_held(Some(8 * GIB), None), None);
    }

    /// What each child gets on the reviewer's small hosts, through the `Ctx`:
    /// no export where the memory holds as many jobs as cargo runs, yet every
    /// side-lane command at or under the figure; and the caller's smaller
    /// export still wins.
    #[test]
    fn a_small_degraded_host_caps_its_side_lanes_without_an_export() {
        const GIB: u64 = disk::GIB;
        let host = |ram: u64, usable: u64, cores: u32, caller: Option<&str>| {
            let mut c = Ctx::new(
                PathBuf::from("/repo"),
                Mode::Fast,
                Scope::Workspace,
                EnvSnapshot::default(),
                PathBuf::from("/tmp"),
            );
            c.env.cargo_build_jobs = caller.map(Into::into);
            c.with_host_memory(Some(ram))
                .with_host_usable(Some(usable))
                .with_memory_build_jobs_on(cores)
        };
        let exported = |c: &Ctx| {
            c.child_env_add
                .iter()
                .rfind(|(k, _)| k == "CARGO_BUILD_JOBS")
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        let driver = |c: &Ctx| {
            let (_, cmd) = stages::driver_build_cmds(c)
                .into_iter()
                .next()
                .expect("a driver build");
            cmd.envs
                .iter()
                .rfind(|(k, _)| k == "CARGO_BUILD_JOBS")
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        let two_core = host(8 * GIB, GIB * 9 / 2, 2, None);
        assert_eq!(exported(&two_core), None, "2 cores already run 2");
        assert_eq!(two_core.build_jobs_ceiling(), Some(2));
        assert_eq!(driver(&two_core).as_deref(), Some("2"));
        let four_core = host(16 * GIB, 12 * GIB, 4, None);
        assert_eq!(exported(&four_core), None, "4 cores, memory holds 6");
        assert_eq!(driver(&four_core).as_deref(), Some("6"));
        // Negative control: the same memory on 8 cores is exported.
        let eight_core = host(8 * GIB, GIB * 9 / 2, 8, None);
        assert_eq!(exported(&eight_core).as_deref(), Some("2"));
        assert_eq!(driver(&eight_core).as_deref(), Some("2"));
        // The caller's 1 is below the memory's 2: it stands everywhere.
        let caller_one = host(8 * GIB, GIB * 9 / 2, 8, Some("1"));
        assert_eq!(exported(&caller_one), None);
        assert_eq!(driver(&caller_one).as_deref(), Some("1"));
        // A healthy host gets neither.
        let healthy = host(8 * GIB, 7 * GIB, 2, None);
        assert_eq!(healthy.memory_jobs, None);
        assert_eq!(exported(&healthy), None);
        assert_eq!(driver(&healthy).as_deref(), Some("8"));
        // The start note says only what the figure changes: the 2-core host's
        // side lanes are lowered, a degraded host whose memory holds more than
        // any lane runs (this 36 GiB Mac with 4.2 GiB wired holds 15) and a
        // caller already under the figure are not.
        assert_eq!(two_core.notes.len(), 1, "{:?}", two_core.notes);
        let roomy = host(36 * GIB, 318 * GIB / 10, 14, None);
        assert!(roomy.memory_jobs.is_some_and(|held| held >= 8));
        assert!(roomy.notes.is_empty(), "{:?}", roomy.notes);
        assert!(caller_one.notes.is_empty(), "{:?}", caller_one.notes);
    }

    /// The time line carries the machine's load when both ends were read.
    #[test]
    fn the_time_line_names_the_load_at_the_stage_s_two_ends() {
        let d = |s: f64| Duration::from_secs_f64(s);
        assert_eq!(
            time_line(d(12.34), d(4.5), None),
            "  time  4.5s (started +12.3s)\n"
        );
        assert_eq!(
            time_line(
                d(12.34),
                d(4.5),
                ladder::StageLoad::of(Some(74.82), Some(80.1), 14)
            ),
            "  time  4.5s (started +12.3s; load 74.82 -> 80.10 on 14 cores)\n"
        );
    }

    #[test]
    fn locate_root_walks_up_to_the_marker_pair() {
        let tmp = mktemp_dir("atv-root").expect("mktemp");
        let root = tmp.join("repo");
        let deep = root.join("crates/aterm-verify/src");
        std::fs::create_dir_all(&deep).expect("mkdir");
        std::fs::create_dir_all(root.join("tools")).expect("mkdir");
        // Only Cargo.toml: not a root yet — both markers are required, so a
        // nested crate manifest can never be mistaken for the workspace.
        std::fs::write(root.join("Cargo.toml"), b"[workspace]\n").expect("write");
        std::fs::write(root.join("crates/aterm-verify/Cargo.toml"), b"[package]\n").expect("write");
        assert_eq!(locate_root(&deep), None);

        std::fs::write(root.join("tools/verify.sh"), b"#!/bin/sh\n").expect("write");
        assert_eq!(locate_root(&deep).as_deref(), Some(root.as_path()));
        assert_eq!(locate_root(&root).as_deref(), Some(root.as_path()));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The unix half of [`is_executable_file`]: a 0644 file is not executable.
    #[cfg(unix)]
    #[test]
    fn executable_test_matches_the_shell_test() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = mktemp_dir("atv-exec").expect("mktemp");
        let f = tmp.join("script.sh");
        std::fs::write(&f, b"#!/bin/sh\n").expect("write");
        assert!(!is_executable_file(&f), "0644 file is not executable");
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(is_executable_file(&f));
        assert!(
            !is_executable_file(&tmp),
            "a directory is not an executable file"
        );
        assert!(!is_executable_file(&tmp.join("absent")));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Two bodies behind ONE inode run as two stand-ins, and a body rewritten
    /// between two runs takes effect: the text is shared, the behaviour is the
    /// data beside it. Non-vacuity: the two print and exit differently while
    /// their inode is one, so a [`standin_body`] that dropped the body, kept
    /// the first one for every path, or gave each path its own text fails here.
    #[cfg(unix)]
    #[test]
    fn a_stand_in_s_body_is_data_behind_one_shared_inode() {
        use std::os::unix::fs::MetadataExt;
        let tmp = mktemp_dir("atv-standin-body").expect("mktemp");
        let run = |p: &Path| {
            let out = Command::new(p)
                .args(["x", "y z"])
                .output()
                .expect("run the stand-in");
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                out.status.code(),
            )
        };
        let (a, b) = (tmp.join("bin/a"), tmp.join("bin/b"));
        standin_body(&a, r#"echo "a ${0##*/} $# $2"; exit 7"#);
        standin_body(&b, r#"echo "b ${0##*/}"; exit 3"#);
        let id = |p: &Path| {
            let m = std::fs::metadata(p).expect("stat");
            (m.dev(), m.ino())
        };
        assert_eq!(id(&a), id(&b), "two bodies, ONE inode");
        // `$0` is the link run, and the arguments are the stand-in's own.
        assert_eq!(run(&a), ("a a 2 y z\n".into(), Some(7)));
        assert_eq!(run(&b), ("b b\n".into(), Some(3)));

        // Rewritten between two runs: the next run reads the new body, the
        // inode (so the stamp a tripwire reads) and the other stand-in stay.
        standin_body(&a, r#"echo "a again"; exit 9"#);
        assert_eq!(run(&a), ("a again\n".into(), Some(9)));
        assert_eq!(id(&a), id(&b), "a rewrite never moves the inode");
        assert_eq!(run(&b), ("b b\n".into(), Some(3)));

        // The data file is renamed into place: nothing else is left beside it.
        let mut names: Vec<_> = std::fs::read_dir(tmp.join("bin"))
            .expect("ls")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a", "a.body", "b", "b.body"]);
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The `not(unix)` half: with no execute bit a plain file is runnable
    /// (`PATHEXT` decides); only the directory and absent cases are false.
    #[cfg(not(unix))]
    #[test]
    fn executable_test_is_existence_where_there_is_no_execute_bit() {
        let tmp = mktemp_dir("atv-exec").expect("mktemp");
        let f = tmp.join("script.sh");
        std::fs::write(&f, b"#!/bin/sh\n").expect("write");
        assert!(
            is_executable_file(&f),
            "no execute bit exists here: a plain file is runnable"
        );
        assert!(
            !is_executable_file(&tmp),
            "a directory is not an executable file"
        );
        assert!(!is_executable_file(&tmp.join("absent")));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The context keeps the toolchain it is handed ([`run_toolchain`]).
    /// Negative control: [`Ctx::new`], which discovers, finds none in an empty
    /// environment.
    #[test]
    fn a_run_discovers_its_toolchain_once_and_hands_it_to_every_consumer() {
        let tmp = mktemp_dir("atv-one-toolchain").expect("mktemp");
        let bin = tmp.join("handed/bin");
        let handed = Toolchain {
            stage2_dir: bin.clone(),
            targo: bin.join("targo"),
            trustdoc: bin.join("trustdoc"),
            tippy: None,
            refused: None,
            store_bin: None,
            demoted: None,
        };
        let ctx = |tools: Option<Toolchain>| {
            let (root, scratch) = (tmp.clone(), tmp.clone());
            let env = EnvSnapshot::default();
            match tools {
                Some(t) => Ctx::new_with_tools(root, Mode::Fast, Scope::Workspace, env, scratch, t),
                None => Ctx::new(root, Mode::Fast, Scope::Workspace, env, scratch),
            }
        };
        assert_eq!(
            ctx(Some(handed.clone())).tools,
            handed,
            "kept, never re-found"
        );
        assert_ne!(
            ctx(None).tools,
            handed,
            "a discovery of its own finds another"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn every_child_inherits_the_wall_clock_ceiling_the_flag_set() {
        // End to end: `--stage-timeout` (parsed by `exec::parse_ceiling`) reaches
        // every stage child through `Ctx::exec_env`.
        let tmp = mktemp_dir("atv-ceiling").expect("mktemp");
        let ctx = || {
            Ctx::new(
                tmp.clone(),
                Mode::Fast,
                Scope::Workspace,
                EnvSnapshot::default(),
                tmp.clone(),
            )
        };
        assert_eq!(
            ctx().exec_env().child_ceiling,
            Some(exec::DEFAULT_CHILD_CEILING),
            "no flag still leaves the backstop in place"
        );
        let moved = exec::parse_ceiling("120").expect("seconds");
        assert_eq!(
            ctx().with_child_ceiling(moved).exec_env().child_ceiling,
            Some(std::time::Duration::from_secs(120))
        );
        let off = exec::parse_ceiling("off").expect("off");
        assert_eq!(
            ctx().with_child_ceiling(off).exec_env().child_ceiling,
            None,
            "and an operator who types `off` gets the old unbounded wait"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The wiring for [`CHILD_ENV`] and [`CHILD_ENV_REMOVED`]: every stage child
    /// is launched with `CARGO_INCREMENTAL=0` and without the caller's
    /// `CARGO_TARGET_DIR`, in a snapshot run and in an in-process context
    /// alike, and nothing the gate REMOVES is also something it sets. A gate
    /// whose children quietly compiled incrementally again would refill the
    /// 55 GB lane this exists to bound, and nothing else in the tree would
    /// notice until the volume did.
    #[test]
    fn every_child_is_launched_with_incremental_compilation_off() {
        let tmp = mktemp_dir("atv-incremental").expect("mktemp");
        let ctx = Ctx::new(
            tmp.clone(),
            Mode::Fast,
            Scope::Workspace,
            EnvSnapshot::default(),
            tmp.clone(),
        );
        let has = |c: &Ctx, key: &str, val: &str| {
            c.exec_env()
                .add_env
                .iter()
                .any(|(k, v)| k == key && v == val)
        };
        assert!(
            has(&ctx, "CARGO_INCREMENTAL", "0"),
            "{:?}",
            ctx.child_env_add
        );
        assert!(
            CHILD_ENV.contains(&("CARGO_INCREMENTAL", "0")),
            "{CHILD_ENV:?}"
        );
        let snap = Ctx::new(
            tmp.clone(),
            Mode::Fast,
            Scope::Workspace,
            EnvSnapshot::default(),
            tmp.clone(),
        )
        .in_snapshot_of(
            tmp.clone(),
            identity::TreeState {
                head: "0".repeat(40),
                dirty: std::collections::BTreeMap::new(),
            },
            Vec::new(),
        );
        assert!(
            has(&snap, "CARGO_INCREMENTAL", "0"),
            "{:?}",
            snap.child_env_add
        );
        // The run's pinned facts join the seeded vector rather than replace it.
        let pinned = Ctx::new(
            tmp.clone(),
            Mode::Fast,
            Scope::Workspace,
            EnvSnapshot::default(),
            tmp.clone(),
        )
        .with_pinned_child_facts(None);
        assert!(
            has(&pinned, "CARGO_INCREMENTAL", "0"),
            "{:?}",
            pinned.child_env_add
        );
        for (k, _) in CHILD_ENV {
            assert!(
                !snap.exec_env().remove_env.contains(&k),
                "{k} is both set and removed"
            );
        }
        for c in [&ctx, &snap, &pinned] {
            assert!(
                c.exec_env().remove_env.contains(&"CARGO_TARGET_DIR"),
                "a caller's target dir reaches a child: {:?}",
                c.child_env_remove
            );
            // The test run passes `-Zunstable-options`, under which libtest
            // reads these: none may reach a child from the caller's shell.
            for k in LIBTEST_UNSTABLE_ENV {
                assert!(
                    c.exec_env().remove_env.contains(&k),
                    "{k} reaches a child, and the test run's libtest reads it: {:?}",
                    c.child_env_remove
                );
            }
        }
        assert_eq!(ctx.disk_floor, None, "a real run estimates");
        assert_eq!(ctx.disk_budget, disk::Budget::MEASURED);
        assert!(ctx.disk_free.is_none(), "and reads the real volume");
        assert!(ctx.only_stages.is_none(), "and runs every planned stage");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A LADDER THAT LEAVES A STAGE OUT NEVER CLAIMS THE CONTRACT
    /// ([`Ctx::with_only_stages`]): each stage it leaves out is one named skip,
    /// and a skip forfeits the claim whatever else the run decided. The list is
    /// empty here, so every row is the seam's own and nothing runs or fails —
    /// as green a ladder as a run can have, which is what a seam that answered
    /// a left-out stage with a pass would have crowned the merge contract.
    #[cfg(unix)]
    #[test]
    fn a_ladder_that_leaves_a_stage_out_never_claims_the_contract() {
        let tmp = mktemp_dir("atv-only-stages").expect("mktemp");
        let scratch = tmp.join("scratch");
        std::fs::create_dir_all(&scratch).expect("mkdir");
        let ctx = Ctx::new(
            tmp.clone(),
            Mode::Fast,
            Scope::Workspace,
            EnvSnapshot::default(),
            scratch,
        )
        .with_disk_floor(0)
        .with_only_stages(&[]);
        let mut out: Vec<u8> = Vec::new();
        let code = run(&ctx, &mut out).expect("the ladder is writable");
        let ladder = String::from_utf8(out).expect("utf-8 ladder");
        std::fs::remove_dir_all(&tmp).ok();

        assert_eq!(code, exit::PASS, "{ladder}");
        assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE), "{ladder}");
        assert!(ladder.contains("NOT the merge contract"), "{ladder}");
        for spec in plan::plan(&ctx) {
            assert!(
                ladder.contains(&format!(
                    "  skip  {} (not in this fixture ladder's stages)",
                    spec.title
                )),
                "{} is not a skip row: {ladder}",
                spec.title
            );
        }
    }

    /// Every child receives the directory discovery settled on as
    /// `$TRUST_STAGE2_BIN`. Driven through a real child, since reaching the
    /// process is the property. Negative control: no `targo`, nothing exported.
    #[cfg(unix)]
    #[test]
    fn every_child_is_handed_the_runs_own_toolchain_as_the_override() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = mktemp_dir("atv-child-tc").expect("mktemp");
        let stage2 = tmp.join("found-on-path/bin");
        std::fs::create_dir_all(&stage2).expect("mkdir");
        std::fs::write(stage2.join("targo"), b"#!/bin/sh\nexit 0\n").expect("write");
        std::fs::set_permissions(stage2.join("targo"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let physical = std::fs::canonicalize(&stage2).expect("canonicalize");
        let ctx = |path: &std::path::Path| {
            let env = EnvSnapshot {
                path: path.as_os_str().to_os_string(),
                home: tmp.join("empty-home"),
                ..EnvSnapshot::default()
            };
            Ctx::new(tmp.clone(), Mode::Fast, Scope::Workspace, env, tmp.clone())
                .with_pinned_child_facts(None)
        };

        let pinned = ctx(&stage2);
        assert!(pinned.tools.have_targo(), "{:?}", pinned.tools);
        assert_eq!(pinned.tools.stage2_dir, physical);
        let seen = tmp.join("seen");
        let run = exec::run(
            &exec::Cmd::new("/bin/sh").arg("-c").arg(format!(
                "printf %s \"$TRUST_STAGE2_BIN\" > '{}'",
                seen.display()
            )),
            pinned.exec_env(),
        );
        assert!(run.ok, "the probe child ran");
        assert_eq!(
            std::fs::read_to_string(&seen).expect("the child wrote"),
            physical.display().to_string(),
            "the child must be handed the run's own resolved directory"
        );

        let none = ctx(&tmp.join("nothing-here"));
        assert!(!none.tools.have_targo());
        assert!(
            !none
                .child_env_add
                .iter()
                .any(|(k, _)| k == "TRUST_STAGE2_BIN"),
            "no toolchain, no override: {:?}",
            none.child_env_add
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn pinned_display_and_native_update_identities_describe_the_same_tree() {
        let tmp = mktemp_dir("atv-git-stamp").expect("mktemp");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&tmp)
                .output()
                .expect("git");
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout)
                .expect("git UTF-8")
                .trim()
                .to_string()
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ]);
        git(&["tag", "v0.1.0"]);
        let full = git(&["rev-parse", "HEAD"]);
        let short = git(&["rev-parse", "--short=12", "HEAD"]);
        for dirty in [false, true] {
            if dirty {
                std::fs::write(tmp.join("untracked"), "changed").expect("dirty fixture");
            }
            let ctx = Ctx::new(
                tmp.clone(),
                Mode::Fast,
                Scope::Workspace,
                EnvSnapshot::default(),
                tmp.clone(),
            )
            .with_pinned_child_facts(None);
            let fact = |key: &str| {
                ctx.child_env_add
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, value)| value.to_string_lossy().into_owned())
                    .expect("pinned fact")
            };
            assert_eq!(fact(GIT_STAMP_ENV[1]), full);
            assert_eq!(
                fact(GIT_STAMP_ENV[0]),
                if dirty {
                    format!("{short}-dirty")
                } else {
                    short.clone()
                }
            );
            assert_eq!(fact(GIT_STAMP_ENV[2]), "0");
        }
        std::fs::remove_dir_all(tmp).expect("fixture cleanup");
    }

    /// The toolchain header names the resolved absolute directory, and says so
    /// plainly when it cannot (a relative path, no toolchain, a demoted rustup
    /// entry).
    #[test]
    fn the_gate_names_the_absolute_toolchain_it_resolved_and_admits_when_it_cannot() {
        let line = toolchain_header_line(
            std::path::Path::new("/Users//x/toolchains/trust-current/bin"),
            Some("trust"),
            true,
        );
        assert!(line.starts_with("verify: toolchain /Users//x/toolchains/trust-current/bin "));
        assert!(line.contains("channel \"trust\""));
        assert!(
            line.contains("resolved once"),
            "the line must say the path is PINNED, not merely print one: {line}"
        );
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1);

        // A relative path is a gate that cannot say which compiler it used.
        let rel = toolchain_header_line(std::path::Path::new("bin"), Some("trust"), true);
        assert!(rel.contains("RELATIVE"), "{rel}");
        assert!(!rel.contains("resolved once"), "{rel}");

        // And no toolchain at all is NONE, never a bare path that reads like one.
        let none = toolchain_header_line(
            std::path::Path::new("/Users//x/trust/build/host/stage2/bin"),
            None,
            false,
        );
        assert!(none.contains("toolchain NONE"), "{none}");
        assert!(none.contains("<unpinned>"), "{none}");

        // A demoted rustup entry is SAID under the header, in one line of its own.
        let note = toolchain_demoted_line(&toolchain::Demoted {
            dir: std::path::PathBuf::from("/Users//x/trust/build/aarch64-apple-darwin/stage2/bin"),
            its: "2026-08-20".into(),
            store: "2026-09-17".into(),
            linked: true,
        });
        assert!(
            note.starts_with("verify: toolchain note — rustup `trust` ("),
            "{note}"
        );
        assert!(
            note.contains("a toolchain from 2026-08-20, older than the atpkg store's 2026-09-17"),
            "{note}"
        );
        assert!(
            note.ends_with('\n') && note.matches('\n').count() == 1,
            "{note}"
        );
    }
}
