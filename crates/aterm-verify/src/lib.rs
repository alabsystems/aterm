// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-verify` — every aterm check as one CLI: `targo --unverified verify [CHECK|SET ...]`.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the verify CLI spawns its checks"
)]

#[cfg(unix)]
pub mod browser;
pub mod cells;
pub mod changed;
pub mod checks;
pub mod cli;
/// The disk a run will write, claimed before it builds (`docs/DESIGN-adaptive-build-2026-10-24.md`, DISK-ADMIT).
#[cfg(unix)]
pub mod disk;
pub mod docdriver;
pub mod docscan;
pub mod exec;
pub mod findings;
pub mod fmt;
pub mod ladder;
pub mod libtest;
pub mod perf;
pub mod plan;
#[cfg(unix)]
pub mod provision;
pub mod report;
/// Nothing outlives its run: the run record, the stage-end survivor check and the next run's start ending a dead run's proven members (`docs/DESIGN-leftover-processes-2026-10-16.md` §3.3, §5.4, §8.1).
pub mod runs;
pub mod rustup;
pub mod sched;
pub mod scope;
#[cfg(unix)]
pub mod smoke;
#[cfg(unix)]
pub mod smoke_stages;
pub mod soak;
pub mod stages;
mod test_workers;
pub mod testrun;
/// THE toolchain resolution, shared with atpkg (`aterm help rust`), the libc oracle's gate and the release cutter.
pub use aterm_toolchain::discovery as toolchain;
pub mod wasm_glue;
mod wasm_page;
pub mod wasm_smoke;

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_capacity::Machine as _;

pub use checks::Check;
pub use ladder::{Outcome, Report, Severity};
pub use scope::Scope;
pub use toolchain::{Toolchain, is_executable_file};

/// Exit codes.
pub mod exit {
    /// Every check that ran passed.
    pub const PASS: i32 = 0;
    /// A check failed.
    pub const FAILED: i32 = 1;
    /// Usage error.
    pub const USAGE: i32 = 2;
    /// Could not run.
    pub const COULD_NOT_RUN: i32 = 3;
}

/// The environment, read once at startup on the main thread.
#[derive(Clone, Debug, Default)]
pub struct EnvSnapshot {
    pub path: OsString,
    pub home: PathBuf,
    pub trust_stage2_bin: Option<PathBuf>,
    /// `XDG_CONFIG_HOME`.
    pub xdg_config_home: Option<PathBuf>,
    /// `RUSTUP_HOME`, raw ([`toolchain::rustup_home`]).
    pub rustup_home: Option<OsString>,
    /// `SSH_CONNECTION`.
    pub ssh_connection: Option<String>,
    /// `CARGO_BUILD_JOBS`, raw.
    pub cargo_build_jobs: Option<OsString>,
    /// `BUILD_SLOT_HELD`, raw: a managed run must name its admitted CPU share.
    pub build_slot_held: Option<OsString>,
    /// `RUSTDOC` or `CARGO_BUILD_RUSTDOC`.
    pub rustdoc_override: Option<OsString>,
    /// `CARGO_TARGET_DIR`, absolute.
    pub cargo_target_dir: Option<PathBuf>,
    /// The variables that choose cargo's layout, raw and only when set ([`CARGO_LAYOUT_VARS`]).
    pub cargo_layout: Vec<(&'static str, OsString)>,
    /// `RUST_TEST_THREADS`, raw.
    pub rust_test_threads: Option<OsString>,
    /// `CARGO_HOME`.
    pub cargo_home: Option<PathBuf>,
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
            build_slot_held: std::env::var_os("BUILD_SLOT_HELD"),
            rustdoc_override: std::env::var_os("RUSTDOC")
                .or_else(|| std::env::var_os("CARGO_BUILD_RUSTDOC")),
            cargo_target_dir: var_path("CARGO_TARGET_DIR")
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| std::path::absolute(&p).unwrap_or(p)),
            cargo_home: var_path("CARGO_HOME").filter(|p| p.is_absolute()),
            cargo_layout: CARGO_LAYOUT_VARS
                .iter()
                .filter_map(|k| Some((*k, std::env::var_os(k)?)))
                .collect(),
            rust_test_threads: std::env::var_os("RUST_TEST_THREADS"),
        }
    }
}

/// The variables that move cargo's target or build directory, and the home whose config may.
pub const CARGO_LAYOUT_VARS: [&str; 4] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_BUILD_DIR",
    "CARGO_HOME",
];

/// The `build.build-dir` value atpkg's `targo` shim on `env`'s `PATH` uses for `root`
/// (`aterm_toolchain::build_cache::decide`), in the cache root that shim names
/// (`aterm_toolchain::build_cache::shim_root`): a shim laid before the cache moved under `.ubd`
/// builds in the old root until an update of aterm lays the new one, and the main lane builds
/// there with it, so plain `targo` and verify share one build.
#[must_use]
pub fn main_build_dir(env: &EnvSnapshot, root: &Path) -> Option<String> {
    use aterm_toolchain::build_cache::{BuildDir, Machine, Tool, decide, read_config, shim_root};
    if !cfg!(target_os = "macos") {
        return None;
    }
    let lookup = |k: &str| {
        env.cargo_layout
            .iter()
            .find(|(name, _)| *name == k)
            .map(|(_, v)| v.clone())
    };
    let cache = shim_root(&env.home, &env.path);
    let machine = Machine {
        home: Some(&env.home),
        root: Some(&cache),
        cwd: Some(root),
        env: &lookup,
        read: &read_config,
        writable: &writable_dir,
    };
    let probe = ["--unverified".to_string(), "test".to_string()];
    match decide(&probe, Tool::Targo, &machine)? {
        BuildDir::Cache(value) => Some(value),
        BuildDir::Target(_) | BuildDir::TrustEnv(_) => None,
    }
}

/// The `build.build-dir` for main-lane children when [`main_build_dir`] is `None` and `target` is pinned.
#[must_use]
pub fn fallback_build_dir(env: &EnvSnapshot, root: &Path, target: &Path) -> Option<String> {
    use aterm_toolchain::build_cache::{Machine, read_config, target_build_dir};
    let lookup = |k: &str| {
        env.cargo_layout
            .iter()
            .find(|(name, _)| *name == k)
            .map(|(_, v)| v.clone())
    };
    let target = std::path::absolute(target).ok()?;
    let machine = Machine {
        home: Some(&env.home)
            .filter(|h| !h.as_os_str().is_empty())
            .map(PathBuf::as_path),
        root: None,
        cwd: Some(root),
        env: &lookup,
        read: &read_config,
        writable: &writable_dir,
    };
    target_build_dir(&target, &machine)
}

/// Whether this process may write the directory `dir` (`access(2)`, which a
/// sandbox's denial answers too, as atpkg's shim asks it); `None` when `dir` is
/// not a directory.
#[cfg(unix)]
fn writable_dir(dir: &Path) -> Option<bool> {
    use std::os::unix::ffi::OsStrExt as _;
    if !dir.is_dir() {
        return None;
    }
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a NUL-terminated string that outlives the call, and `access` only reads it.
    Some(unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
}

/// Off unix the shim's cache is never chosen ([`main_build_dir`] is macOS only).
#[cfg(not(unix))]
fn writable_dir(_dir: &Path) -> Option<bool> {
    None
}

/// The release bundle `measure` judges (`--app <APP>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureArtifact {
    /// The bundle, canonical.
    pub app: PathBuf,
    /// Its `CFBundleShortVersionString`, when its `Info.plist` names one.
    pub version: Option<String>,
    /// The commit it was built from (`ATermGitCommit`: 12 hex digits, `-dirty` when that tree had
    /// changes), when its `Info.plist` names one.
    pub commit: Option<String>,
}

impl MeasureArtifact {
    /// The bundle at `app`.
    ///
    /// # Errors
    /// What `app` is instead, in words.
    pub fn at(app: &Path) -> Result<Self, String> {
        let app = std::fs::canonicalize(app)
            .map_err(|e| format!("{} is not a release bundle: {e}", app.display()))?;
        if !is_executable_file(&app.join("Contents/MacOS/aterm")) {
            return Err(format!(
                "{} is not a release bundle: it holds no executable Contents/MacOS/aterm",
                app.display()
            ));
        }
        let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap_or_default();
        let read = |key| plist_string(&plist, key).map(str::to_string);
        Ok(Self {
            version: read("CFBundleShortVersionString"),
            commit: read("ATermGitCommit"),
            app,
        })
    }

    /// The bundle at `app`, naming neither a version nor a commit.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn unnamed(app: impl Into<PathBuf>) -> Self {
        Self {
            app: app.into(),
            version: None,
            commit: None,
        }
    }

    /// `<app>/Contents/MacOS/<name>`.
    #[must_use]
    pub fn bin(&self, name: &str) -> PathBuf {
        self.app.join("Contents/MacOS").join(name)
    }

    /// Whether `head` (a full commit id) built it: its commit, less `-dirty`, begins `head`.
    #[must_use]
    pub fn built_from(&self, head: &str) -> bool {
        self.commit.as_deref().is_some_and(|c| {
            let c = c.strip_suffix("-dirty").unwrap_or(c);
            !c.is_empty() && head.starts_with(c)
        })
    }

    /// `aterm <version> (<commit>)` as far as its `Info.plist` says, then where it is.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut name = String::from("aterm");
        if let Some(v) = &self.version {
            name.push(' ');
            name.push_str(v);
        }
        if let Some(c) = &self.commit {
            name.push_str(&format!(" ({c})"));
        }
        format!("{name} at {}", self.app.display())
    }
}

/// The `<string>` after `<key>key</key>` in an XML property list.
fn plist_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let key = format!("<key>{key}</key>");
    let tail = text[text.find(&key)? + key.len()..]
        .trim_start()
        .strip_prefix("<string>")?;
    let value = tail[..tail.find("</string>")?].trim();
    (!value.is_empty()).then_some(value)
}

/// Everything a stage needs, immutable for the whole run so stages share it across threads without a lock.
#[derive(Debug)]
pub struct Ctx {
    pub root: PathBuf,
    /// The caller's `CARGO_TARGET_DIR`, else `<root>/target`.
    pub target_dir: PathBuf,
    /// The checks this run runs, in [`checks::CHECKS`] order.
    pub checks: Vec<Check>,
    /// The `release` set.
    pub release: bool,
    /// `--fix`: `fmt` rewrites the files it finds unformatted.
    pub fix: bool,
    /// `--cell`: the forge cells `cells` checks.
    pub cells: Vec<String>,
    /// The selection pulled `cells` ([`checks::pulls_cells`]): one cell per other OS, each
    /// checked only where its graph holds a selected crate ([`cells::pulled_cells`]).
    pub cells_pulled: bool,
    pub scope: Scope,
    pub tools: Toolchain,
    /// PATH for every child.
    pub path_env: OsString,
    /// Private scratch directory for captured child output.
    pub scratch: PathBuf,
    pub env: EnvSnapshot,
    /// Variables removed from every child's environment ([`CHILD_ENV_REMOVED`]).
    pub child_env_remove: Vec<&'static str>,
    /// Variables given to every child, after the removals and before a stage's own ([`exec::ExecEnv::add_env`]).
    pub child_env_add: Vec<(OsString, OsString)>,
    /// The `build.build-dir` the main lane's children are handed as `--config`.
    pub main_build_dir: Option<String>,
    /// What the run's toolchain lease is, for the log ([`hold_toolchain`]).
    pub toolchain_lease: Option<String>,
    /// HEAD, in full, when the run's selection was read ([`report::moved_checkout`]); unset, the
    /// run reads it as it begins.
    pub head_at_start: Option<String>,
    /// The ceiling on one stage child's work, its waits for admission or a lock left out ([`exec::ExecEnv::child_ceiling`]).
    pub child_ceiling: Option<Duration>,
    /// `--no-windows`.
    pub no_windows: bool,
    /// `--app <APP>`.
    pub measure_artifact: Option<MeasureArtifact>,
    /// Requested `--test-jobs` ceiling; [`Self::test_jobs`] applies admission.
    pub test_jobs: u32,
    /// Requested `--test-threads`, else the caller's own `RUST_TEST_THREADS` ceiling.
    pub test_threads: Option<u32>,
    /// The program cargo runs in place of test binaries ([`testrun::RECORD_FLAG`]) and rustdoc ([`docdriver`]).
    pub test_recorder: Option<PathBuf>,
    /// How the timing stages ([`plan::TIMED`]) take the quiet machine: the run waits in the
    /// per-user capacity ledger for one quiet claim before its first (`Some(Wait)`), or takes it
    /// at once (`Some(Now)`, `--admit now`), and holds it until its last ends; `None` runs them
    /// with no claim.
    pub quiet: Option<aterm_capacity::Admit>,
    /// This run's record of its stage groups ([`runs::Runs`]), when it keeps one.
    pub runs: Option<runs::Runs>,
}

/// A `CARGO_BUILD_JOBS` value as a job count.
#[must_use]
pub fn parse_build_jobs(raw: Option<&OsStr>) -> Option<u32> {
    raw.and_then(OsStr::to_str)
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| n > 0)
}

/// What `cargo run` hands the verify process about ITSELF (the `verify` alias is `run -q -p aterm-verify`): `aterm_toolchain::build_cache::CARGO_RUN_ENV`.
pub const CARGO_RUN_ENV: [&str; 7] = aterm_toolchain::build_cache::CARGO_RUN_ENV;

/// Variables removed from every child's inherited environment.
pub const CHILD_ENV_REMOVED: [&str; 7] = CARGO_RUN_ENV;

/// The run's toolchain, discovered once from `env` under the pin `root` declares and the atpkg prefix atpkg itself would read.
#[must_use]
pub fn run_toolchain(env: &EnvSnapshot, root: &Path) -> Toolchain {
    let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    Toolchain::discover_with_store(
        env.trust_stage2_bin.as_deref(),
        &toolchain::rustup_home(env.rustup_home.as_deref(), &env.home),
        Some(&prefix),
        &env.path,
        toolchain::pinned_channel(root).as_deref(),
    )
}

impl Ctx {
    /// The run context, discovering its toolchain ([`run_toolchain`]).
    #[must_use]
    pub fn new(root: PathBuf, scope: Scope, env: EnvSnapshot, scratch: PathBuf) -> Self {
        let tools = run_toolchain(&env, &root);
        Self::new_with_tools(root, scope, env, scratch, tools)
    }

    /// [`Self::new`] with the run's toolchain already discovered.
    #[must_use]
    pub fn new_with_tools(
        root: PathBuf,
        scope: Scope,
        env: EnvSnapshot,
        scratch: PathBuf,
        tools: Toolchain,
    ) -> Self {
        let path_env = tools.path_with_stage2_first(&env.path);
        let target_dir = env
            .cargo_target_dir
            .clone()
            .unwrap_or_else(|| root.join("target"));
        let shim_build_dir = main_build_dir(&env, &root);
        let mut child_env_add: Vec<(OsString, OsString)> = Vec::new();
        if shim_build_dir.is_none() {
            child_env_add.push((
                "CARGO_TARGET_DIR".into(),
                target_dir.clone().into_os_string(),
            ));
        }
        let main_build_dir =
            shim_build_dir.or_else(|| fallback_build_dir(&env, &root, &target_dir));
        if tools.have_targo() {
            child_env_add.push((
                "TRUST_STAGE2_BIN".into(),
                tools.stage2_dir.clone().into_os_string(),
            ));
        }
        let pinned_threads = parse_build_jobs(env.rust_test_threads.as_deref());
        let mut ctx = Self {
            root,
            target_dir,
            checks: checks::DEV.to_vec(),
            release: false,
            fix: false,
            cells: Vec::new(),
            cells_pulled: false,
            scope,
            tools,
            path_env,
            scratch,
            env,
            child_env_remove: CHILD_ENV_REMOVED.to_vec(),
            child_env_add,
            main_build_dir,
            toolchain_lease: None,
            head_at_start: None,
            child_ceiling: Some(exec::DEFAULT_CHILD_CEILING),
            no_windows: false,
            measure_artifact: None,
            test_jobs: testrun::DEFAULT_JOBS,
            test_threads: pinned_threads,
            test_recorder: std::env::current_exe().ok(),
            quiet: None,
            runs: None,
        };
        ctx.refresh_test_threads();
        ctx
    }

    /// Keep this run's leftover record ([`runs::Runs`]) and set `ATERM_VERIFY_RUN` on every stage child (W-run).
    #[must_use]
    pub fn with_runs(mut self, runs: runs::Runs) -> Self {
        self.child_env_add
            .push((runs::VERIFY_RUN_ENV.into(), runs.env_value().into()));
        self.runs = Some(runs);
        self
    }

    /// On macOS, every stage child's `TMPDIR` is the temp directory `temp`'s quiet folder
    /// ([`aterm_tempfile::quiet_in`]), so what the tests and tools write there reaches none of
    /// fseventsd's clients or its history; nothing changes where `temp` is quiet already or the
    /// folder cannot be used. A stage's own `TMPDIR` still wins.
    #[must_use]
    pub fn with_quiet_tmpdir(mut self, temp: &Path) -> Self {
        let quiet = aterm_tempfile::quiet_in(temp);
        if quiet != temp {
            let mut dir = quiet.into_os_string();
            dir.push("/");
            self.child_env_add.push(("TMPDIR".into(), dir));
        }
        self
    }

    /// Run `checks` ([`Ctx::checks`]), kept in [`checks::CHECKS`] order.
    #[must_use]
    pub fn with_checks(mut self, wanted: &[Check]) -> Self {
        self.checks = checks::CHECKS
            .iter()
            .map(|s| s.check)
            .filter(|c| wanted.contains(c))
            .collect();
        self
    }

    /// The `release` set ([`Ctx::release`]).
    #[must_use]
    pub const fn with_release(mut self, release: bool) -> Self {
        self.release = release;
        self
    }

    /// `--fix` ([`Ctx::fix`]).
    #[must_use]
    pub const fn with_fix(mut self, fix: bool) -> Self {
        self.fix = fix;
        self
    }

    /// `--cell` ([`Ctx::cells`]).
    #[must_use]
    pub fn with_cells(mut self, cells: Vec<String>) -> Self {
        self.cells = cells;
        self
    }

    /// Whether the selection pulled `cells` ([`Ctx::cells_pulled`]).
    #[must_use]
    pub const fn with_cells_pulled(mut self, pulled: bool) -> Self {
        self.cells_pulled = pulled;
        self
    }

    /// The cargo the in-process forge walks ask for graphs.
    #[must_use]
    pub fn forge_cargo(&self) -> aterm_forge::resolve::Cargo {
        aterm_forge::resolve::Cargo::pinned(&self.tools.targo, &self.path_env)
    }

    /// The run's own corner of the target dir (its logs, the foreign cells).
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.target_dir.join("verify")
    }

    /// The machine's parallelism.
    #[must_use]
    pub fn cores(&self) -> u32 {
        std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX))
    }

    /// The most compiler jobs a side lane may take.
    #[must_use]
    pub fn build_jobs_ceiling(&self) -> Option<u32> {
        parse_build_jobs(self.env.cargo_build_jobs.as_deref())
    }

    /// The most test binaries that run at once ([`Ctx::test_jobs`], at least one).
    #[must_use]
    pub fn test_jobs(&self) -> usize {
        usize::try_from(self.test_workers().0).unwrap_or(1)
    }

    /// The requested libtest count, capped together with the binary count by a managed run's CPU share.
    #[must_use]
    pub fn test_threads(&self) -> u32 {
        self.test_workers().1
    }

    fn test_workers(&self) -> (u32, u32) {
        // Invalid managed input is refused before any stage runs. A context
        // inspected before that refusal still cannot advertise an unbounded pool.
        let share = test_workers::admitted_share(&self.env).unwrap_or(Some(1));
        test_workers::counts(
            self.test_jobs,
            self.test_threads.unwrap_or_else(|| self.cores()),
            share,
        )
    }

    pub(crate) fn test_thread_override(&self) -> Option<u32> {
        (self.env.build_slot_held.is_some() || self.test_threads.is_some())
            .then(|| self.test_threads())
    }

    fn refresh_test_threads(&mut self) {
        self.child_env_add
            .retain(|(key, _)| key != "RUST_TEST_THREADS");
        if let Some(threads) = self.test_thread_override() {
            self.child_env_add
                .push(("RUST_TEST_THREADS".into(), threads.to_string().into()));
        }
    }

    /// The recording runner and the doctests' doc driver.
    #[must_use]
    pub fn test_recorder(&self) -> Option<PathBuf> {
        self.test_recorder
            .clone()
            .or_else(|| std::env::current_exe().ok())
    }

    /// `--test-jobs <n>`.
    #[must_use]
    pub fn with_test_jobs(mut self, jobs: Option<std::num::NonZeroU32>) -> Self {
        if let Some(n) = jobs {
            self.test_jobs = n.get();
        }
        self.refresh_test_threads();
        self
    }

    /// `--test-threads <n>`.
    #[must_use]
    pub fn with_test_threads(mut self, threads: Option<std::num::NonZeroU32>) -> Self {
        if let Some(n) = threads {
            self.test_threads = Some(n.get());
        }
        self.refresh_test_threads();
        self
    }

    /// Move the stage-child ceiling (`--timeout`; `None` is `off`).
    #[must_use]
    pub const fn with_child_ceiling(mut self, ceiling: Option<Duration>) -> Self {
        self.child_ceiling = ceiling;
        self
    }

    /// `--no-windows`.
    #[must_use]
    pub const fn with_no_windows(mut self, no_windows: bool) -> Self {
        self.no_windows = no_windows;
        self
    }

    /// `--app <APP>`.
    #[must_use]
    pub fn with_measure_artifact(mut self, artifact: Option<MeasureArtifact>) -> Self {
        self.measure_artifact = artifact;
        self
    }

    /// What the run's toolchain lease is ([`hold_toolchain`]), for the log.
    #[must_use]
    pub fn with_toolchain_lease(mut self, said: Option<String>) -> Self {
        self.toolchain_lease = said;
        self
    }

    /// HEAD when the run's selection was read ([`Ctx::head_at_start`]).
    #[must_use]
    pub fn with_head_at_start(mut self, head: Option<String>) -> Self {
        self.head_at_start = head;
        self
    }

    /// Take the quiet machine around the timing stages, admitted as `mode` says ([`Ctx::quiet`]).
    #[must_use]
    pub const fn with_quiet(mut self, mode: Option<aterm_capacity::Admit>) -> Self {
        self.quiet = mode;
        self
    }

    /// The command environment.
    #[must_use]
    pub fn exec_env(&self) -> exec::ExecEnv<'_> {
        exec::ExecEnv {
            cwd: &self.root,
            path: &self.path_env,
            scratch: &self.scratch,
            child_ceiling: self.child_ceiling,
            remove_env: &self.child_env_remove,
            add_env: &self.child_env_add,
            stage_groups: self.runs.as_ref().map(|r| r as &dyn exec::StageGroupEnd),
        }
    }

    /// The command that reruns `check` alone over this run's selection.
    #[must_use]
    pub fn rerun(&self, check: Check) -> String {
        let mut cmd = format!("targo --unverified verify {}", check.name());
        if check.narrows() {
            match &self.scope {
                Scope::Workspace => cmd.push_str(" --all"),
                s if s.selects_nothing() => {}
                s => {
                    for c in s.crates() {
                        cmd.push_str(" -p ");
                        cmd.push_str(c);
                    }
                }
            }
        }
        if self.no_windows && check.spec().stages.iter().any(|id| id.opens_windows()) {
            cmd.push_str(" --no-windows");
        }
        if check == Check::Measure
            && let Some(a) = &self.measure_artifact
        {
            cmd.push_str(&format!(" --app {}", a.app.display()));
        }
        if check == Check::Cells {
            for cell in &self.cells {
                cmd.push_str(&format!(" --cell {cell}"));
            }
        }
        cmd
    }
}

/// How a timed stage's quiet claim is admitted: [`aterm_capacity::admit`], or a test's.
type AdmitQuiet = fn(
    aterm_capacity::Need,
    aterm_capacity::Admit,
) -> Result<aterm_capacity::Claim, aterm_capacity::Error>;

/// What runs a stage and admits the run's quiet machine: the live ones ([`LIVE`]), or a test's
/// stand-ins.
#[derive(Clone, Copy)]
struct Seams {
    stage: fn(&Ctx, &plan::StageSpec) -> Report,
    admit_quiet: AdmitQuiet,
}

/// The stages themselves, and the per-user capacity ledger on the live machine.
const LIVE: Seams = Seams {
    stage: stages::run_stage,
    admit_quiet: aterm_capacity::admit,
};

/// The run's quiet machine ([`Ctx::quiet`]): one claim, admitted before the run's first timed
/// stage and let go when its last ends, so another run's timed stages wait for all of this run's
/// rather than taking turns with them.
struct Quiet {
    /// The claim, once admitted.
    claim: Option<aterm_capacity::Claim>,
    /// The run's timed stages still to end ([`takes_quiet`]).
    left: usize,
    /// How the claim is admitted.
    admit: AdmitQuiet,
}

impl Quiet {
    /// Room for the timed stages of `stages` that `ctx` runs, admitted by `admit`.
    fn of(ctx: &Ctx, stages: &[plan::StageSpec], admit: AdmitQuiet) -> Mutex<Quiet> {
        Mutex::new(Quiet {
            claim: None,
            left: stages.iter().filter(|s| takes_quiet(ctx, s.id)).count(),
            admit,
        })
    }

    /// One timed stage ended, or never ran: the claim goes with the last.
    fn passed(&mut self) {
        self.left = self.left.saturating_sub(1);
        if self.left == 0 {
            self.claim = None;
        }
    }
}

/// Whether the run is ending on an interrupt (`exec::group::stopping`): its own handler's, or
/// one that ended a wait for capacity (`exec::group::stop_run`). No stage starts after it.
#[cfg(unix)]
fn stopping() -> bool {
    exec::group::stopping()
}

/// Whether the run is ending on an interrupt: never off Unix, where no handler ends it.
#[cfg(not(unix))]
const fn stopping() -> bool {
    false
}

/// Why a stage the run's end kept from starting did not run.
const ENDING: &str = "the run was interrupted before this stage started, so it decided nothing";

/// What the run's quiet claim did for a timed stage, for its time line: `first` when the stage's
/// admission made it. A claim whose patience ran out says it started at its bound, under what
/// load, and how long it reserved the machine ([`aterm_capacity::QuietStart`]).
fn admission(claim: &aterm_capacity::Claim, mode: aterm_capacity::Admit, first: bool) -> String {
    let bound = claim.record().need.quiet.unwrap_or_default();
    let how = claim
        .quiet()
        .filter(|q| q.at_bound || !q.reserved.is_zero())
        .map_or_else(String::new, |q| format!(", {q}"));
    match mode {
        aterm_capacity::Admit::Now => {
            format!("admitted by --admit now (quiet is load <= {bound:.1})")
        }
        aterm_capacity::Admit::Wait if !first => {
            format!("still on the run's quiet machine (load <= {bound:.1}{how})")
        }
        aterm_capacity::Admit::Wait if claim.waited() >= Duration::from_secs(1) => format!(
            "waited {} for a quiet machine (load <= {bound:.1}){how}",
            aterm_capacity::span(claim.waited())
        ),
        aterm_capacity::Admit::Wait => format!("a quiet machine (load <= {bound:.1})"),
    }
}

/// Run a timed stage on the run's quiet machine ([`Quiet`]): the first one's claim is admitted
/// before it starts, waiting for the load within its patience ([`aterm_capacity::QUIET_PATIENCE`])
/// and saying once a minute what it waits for, and let go when the last one's `f` returns. A
/// ledger that cannot be used is said, and the stage runs unadmitted, as a run with no claim does.
/// Also what the claim did, for the stage's time line.
///
/// # Errors
/// Why `f` never ran: an interrupt ended the wait, which ends the run
/// (`exec::group::stop_run`: no stage child starts after it, and each live one is ended, so the
/// stages it cut short or kept from starting are COULD NOT RUN); or the run was already ending
/// ([`stopping`]), and no wait begins.
fn quietly<T>(
    ctx: &Ctx,
    quiet: &Mutex<Quiet>,
    title: &str,
    f: impl FnOnce() -> T,
) -> Result<(T, Option<String>), String> {
    let Some(mode) = ctx.quiet else {
        return if stopping() {
            Err(ENDING.to_owned())
        } else {
            Ok((f(), None))
        };
    };
    let mut q = quiet.lock().unwrap_or_else(PoisonError::into_inner);
    let said = if stopping() {
        Err(None)
    } else {
        match &q.claim {
            Some(claim) => Ok(admission(claim, mode, false)),
            None => match (q.admit)(
                aterm_capacity::Need::timing(format!("verify {title}")),
                mode,
            ) {
                Ok(claim) => {
                    let said = admission(&claim, mode, true);
                    q.claim = Some(claim);
                    Ok(said)
                }
                Err(e @ aterm_capacity::Error::Stopped { .. }) => Err(Some(e)),
                Err(e) => {
                    eprintln!("verify: {e}; {title} runs unadmitted");
                    Ok(format!("unadmitted: {e}"))
                }
            },
        }
    };
    let said = match said {
        Ok(said) => said,
        Err(stopped) => {
            q.passed();
            drop(q);
            let Some(e) = stopped else {
                return Err(ENDING.to_owned());
            };
            eprintln!(
                "verify: {title}: the run's quiet claim was {e}; the run ends, and what it did \
                 not decide COULD NOT RUN"
            );
            #[cfg(unix)]
            exec::group::stop_run();
            return Err(format!(
                "its quiet claim was {e}; it started nothing, and the run ends"
            ));
        }
    };
    drop(q);
    let out = f();
    quiet
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .passed();
    Ok((out, Some(said)))
}

/// The ` time ` line under a stage in the log: how long it ran, when it started, and `machine`,
/// what the machine did meanwhile ([`aterm_capacity::between`]) and how a timed stage was admitted.
#[must_use]
pub fn time_line(begun: Duration, ran: Duration, machine: &str) -> String {
    let mut line = format!(
        "  time  {:.1}s (started +{:.1}s)",
        ran.as_secs_f64(),
        begun.as_secs_f64()
    );
    if !machine.is_empty() {
        line.push_str("; ");
        line.push_str(machine);
    }
    line.push('\n');
    line
}

/// Whether stage `id` runs on the quiet machine: a timed stage ([`plan::TIMED`]) that runs, not
/// one that is a named skip before it starts anything (a windowed stage under `--no-windows`, a
/// measuring one with no `--app`).
fn takes_quiet(ctx: &Ctx, id: plan::StageId) -> bool {
    let needs_app = matches!(id, plan::StageId::MeasuringTests | plan::StageId::GuiSmoke);
    id.timed()
        && !(ctx.no_windows && id.no_windows_skip().is_some())
        && !(needs_app && ctx.measure_artifact.is_none())
}

/// One stage, run by `stage` on the run's quiet machine when it is timed ([`quietly`]), with the
/// machine read as it starts and as it ends: the report, when it started (after any admission
/// wait) and what the machine did meanwhile. A stage the run's end keeps from starting
/// ([`stopping`]), or whose quiet claim an interrupt ended, is COULD NOT RUN.
fn staged(
    ctx: &Ctx,
    quiet: &Mutex<Quiet>,
    spec: &plan::StageSpec,
    stage: fn(&Ctx, &plan::StageSpec) -> Report,
) -> (Report, Instant, String) {
    let run = || {
        let begun = Instant::now();
        let from = aterm_capacity::Live.read(None);
        let report = stage(ctx, spec);
        let to = aterm_capacity::Live.read(None);
        (report, begun, aterm_capacity::between(&from, &to))
    };
    let ran = if takes_quiet(ctx, spec.id) {
        quietly(ctx, quiet, &spec.title, run)
    } else if stopping() {
        Err(ENDING.to_owned())
    } else {
        Ok((run(), None))
    };
    let ((report, begun, mut machine), admitted) = match ran {
        Ok(ran) => ran,
        Err(why) => {
            let mut report = Report::new(spec.title.clone());
            report.cannot_run(format!("{} — {why}", spec.title));
            return (report, Instant::now(), String::new());
        }
    };
    if let Some(said) = admitted {
        machine.push_str("; ");
        machine.push_str(&said);
    }
    (report, begun, machine)
}

/// The one header line.
#[must_use]
pub fn header(ctx: &Ctx, cone: Option<&str>) -> String {
    let own: Vec<Check> = ctx
        .checks
        .iter()
        .copied()
        .filter(|c| !(ctx.cells_pulled && *c == Check::Cells))
        .collect();
    let names: Vec<&str> = own.iter().map(|c| c.name()).collect();
    let holds = |p: &str| ctx.scope.includes_crate(p);
    let pulled = checks::pulled(&checks::DEV, holds);
    let mut what = if ctx.release {
        "release".to_string()
    } else if own == checks::DEV {
        "dev".to_string()
    } else if !pulled.is_empty() && own == checks::with_pulled(&checks::DEV, false, holds) {
        let (added, by): (Vec<&str>, Vec<&str>) =
            pulled.iter().map(|(c, p)| (c.name(), *p)).unzip();
        format!(
            "dev + {} (the selection holds {})",
            added.join(" + "),
            by.join(", ")
        )
    } else {
        names.join(" ")
    };
    if ctx.cells_pulled {
        what.push_str(" + cells (each other OS's cell that builds a selected crate)");
    }
    format!("verify: {what} — {}", selection(ctx, cone))
}

/// What the run's checks read.
#[must_use]
pub fn selection(ctx: &Ctx, cone: Option<&str>) -> String {
    if !ctx.checks.iter().any(|c| c.narrows()) {
        "the whole tree".to_string()
    } else if let Some(cone) = cone {
        cone.to_string()
    } else if ctx.scope.is_workspace() {
        "the whole workspace".to_string()
    } else {
        ctx.scope.label()
    }
}

type Clock = (Duration, Duration, String);

struct Progress {
    /// Stages still to finish, and the first start and last end seen.
    left: usize,
    started: Option<Instant>,
    ended: Option<Instant>,
    reports: Vec<Option<Report>>,
}

/// How a run presents itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct Show<'a> {
    /// What the header says about the selection when it is the change cone.
    pub cone: Option<&'a str>,
    /// Where the full log is, as the caller should read it.
    pub log: Option<&'a str>,
}

/// Run the selected checks and print their lines to `out` as each finishes, then the verdict line and the log's path.
///
/// # Errors
/// Propagates write failures on `out`.
pub fn run(
    ctx: &Ctx,
    out: &mut (dyn Write + Send),
    log: &mut (dyn Write + Send),
    show: &Show<'_>,
) -> std::io::Result<i32> {
    run_with(ctx, out, log, show, LIVE)
}

/// [`run`], its stages run and its quiet machine admitted by `seams`.
fn run_with(
    ctx: &Ctx,
    out: &mut (dyn Write + Send),
    log: &mut (dyn Write + Send),
    show: &Show<'_>,
    seams: Seams,
) -> std::io::Result<i32> {
    if let Err(why) = test_workers::admitted_share(&ctx.env) {
        writeln!(out, "verify: {why}; no check was run")?;
        let _ = writeln!(log, "verify: {why}; no check was run");
        return Ok(exit::USAGE);
    }
    let cone = show.cone;
    let started = Instant::now();
    let head_at_start = ctx
        .head_at_start
        .clone()
        .or_else(|| head_commit(&ctx.root, &ctx.path_env));
    let head = header(ctx, cone);
    writeln!(out, "{head}")?;
    out.flush()?;
    let _ = writeln!(log, "{head}");
    let _ = write!(log, "{}", toolchain_lines(ctx));

    let stages = plan::plan(ctx);
    let owners: Vec<Option<Check>> = stages
        .iter()
        .map(|s| checks::owner(s.id, &ctx.checks))
        .collect();
    let progress: Vec<Mutex<Progress>> = ctx
        .checks
        .iter()
        .map(|c| {
            Mutex::new(Progress {
                left: owners.iter().filter(|o| **o == Some(*c)).count(),
                started: None,
                ended: None,
                reports: vec![None; stages.len()],
            })
        })
        .collect();
    let width = ctx.checks.iter().map(|c| c.name().len()).max().unwrap_or(4);
    let results: Mutex<Vec<report::CheckResult>> = Mutex::new(Vec::new());
    let out = Mutex::new(out);
    let t0 = Instant::now();
    let clocks: Mutex<Vec<Option<Clock>>> = Mutex::new(vec![None; stages.len()]);
    let quiet = Quiet::of(ctx, &stages, seams.admit_quiet);
    let mut log_err = None;

    sched::run_stages(
        &stages,
        |spec| {
            let i = stages
                .iter()
                .position(|s| std::ptr::eq(s, spec))
                .unwrap_or_default();
            let owner = owners[i].and_then(|c| ctx.checks.iter().position(|k| *k == c));
            let (report, begun_at, machine) = staged(ctx, &quiet, spec, seams.stage);
            if let Some(k) = owner
                && let Ok(mut p) = progress[k].lock()
            {
                p.started = Some(p.started.map_or(begun_at, |s| s.min(begun_at)));
            }
            if let Ok(mut c) = clocks.lock() {
                c[i] = Some((
                    begun_at.saturating_duration_since(t0),
                    begun_at.elapsed(),
                    machine,
                ));
            }
            if let Some(k) = owner
                && let Ok(mut p) = progress[k].lock()
            {
                p.reports[i] = Some(report.clone());
                p.ended = Some(Instant::now());
                p.left = p.left.saturating_sub(1);
                if p.left == 0 {
                    let check = ctx.checks[k];
                    let seconds = match (p.started, p.ended) {
                        (Some(a), Some(b)) => b.saturating_duration_since(a).as_secs_f64(),
                        _ => 0.0,
                    };
                    let mine: Vec<&Report> = p.reports.iter().flatten().collect();
                    let scope = if check.narrows() {
                        ctx.scope.label()
                    } else {
                        String::new()
                    };
                    let result = report::judge(check, &mine, seconds, &scope, ctx.rerun(check));
                    let text = report::render(&result, width);
                    if let Ok(mut o) = out.lock() {
                        let _ = o.write_all(text.as_bytes()).and_then(|()| o.flush());
                    }
                    if let Ok(mut r) = results.lock() {
                        r.push(result);
                    }
                }
            }
            report
        },
        |i, report| {
            let clock = clocks.lock().ok().and_then(|c| c.get(i).cloned().flatten());
            let mut text = report.render();
            if let Some((begun, ran, machine)) = clock {
                text.push_str(&time_line(begun, ran, &machine));
            }
            if log_err.is_none()
                && let Err(e) = log.write_all(text.as_bytes())
            {
                log_err = Some(e);
            }
        },
    );
    let out = out
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    results.sort_by_key(|r| r.check);
    if let Some(runs) = &ctx.runs {
        for line in runs.end_of_run(&runs::LiveKernel) {
            let _ = writeln!(log, "verify: {line}");
            writeln!(out, "verify: {line}")?;
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    let (line, code) = report::moved_checkout(
        report::verdict(&results, ctx.release, seconds),
        head_at_start.as_deref(),
        head_commit(&ctx.root, &ctx.path_env).as_deref(),
    );
    let _ = writeln!(log, "\n{line}");
    writeln!(out, "{line}")?;
    if let Some(path) = show.log {
        writeln!(out, "log: {path}")?;
    }
    out.flush()?;
    if let Some(e) = log_err {
        eprintln!("verify: the log could not be written: {e}");
    }
    Ok(code)
}

fn toolchain_lines(ctx: &Ctx) -> String {
    let mut s = format!(
        "toolchain: {} (channel {})\n",
        ctx.tools.stage2_dir.display(),
        toolchain::pinned_channel(&ctx.root)
            .as_deref()
            .unwrap_or("<unpinned>")
    );
    if let Some(d) = &ctx.tools.demoted {
        s.push_str(&format!("toolchain note: {}\n", d.sentence()));
    }
    if let Some(note) = ctx.tools.borrowed_note() {
        s.push_str(&format!("toolchain note: {note}\n"));
    }
    if let Some(lease) = &ctx.toolchain_lease {
        s.push_str(&format!("toolchain lease: {lease}\n"));
    }
    let pinned = ctx
        .child_env_add
        .iter()
        .any(|(k, _)| k == "CARGO_TARGET_DIR");
    match (&ctx.main_build_dir, pinned) {
        (Some(value), false) if value.contains(&format!("/{}/", aterm_tempfile::QUIET)) => {
            s.push_str(&format!(
                "build cache: the main lane shares plain targo's ({value})\n"
            ));
        }
        (Some(value), false) => s.push_str(&format!(
            "build cache: the main lane shares plain targo's ({value}), outside .ubd: the targo \
             shim on PATH predates the quiet cache, and an aterm update lays the new one\n"
        )),
        (Some(value), true) => s.push_str(&format!(
            "build cache: CARGO_TARGET_DIR={}, intermediates out of Spotlight ({value}; no shim cache here)\n",
            ctx.target_dir.display()
        )),
        (None, _) => s.push_str(&format!(
            "build cache: CARGO_TARGET_DIR={} (no shim cache here; the caller chose the build directory)\n",
            ctx.target_dir.display()
        )),
    }
    s
}

/// THE RUN'S TOOLCHAIN LEASES (`aterm_toolchain::lease`).
#[must_use]
pub fn hold_toolchain(
    env: &EnvSnapshot,
    tools: &Toolchain,
    root: &Path,
) -> (Vec<aterm_toolchain::lease::Lease>, Option<String>) {
    use aterm_toolchain::lease;
    let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    let who = format!(
        "aterm-verify (pid {}) \u{2014} a verify run in {}",
        std::process::id(),
        root.display()
    );
    let mut held: Vec<lease::Lease> = Vec::new();
    for dir in held_dirs(tools) {
        let Some(subject) = lease::Subject::of_dir(&prefix, &dir) else {
            continue;
        };
        if held.iter().any(|l| *l.subject() == subject) {
            continue;
        }
        match lease::take(&prefix, &subject, &who, lease::DEFAULT_WAIT) {
            Ok(l) => held.push(l),
            Err(e) => eprintln!(
                "verify: warn \u{2014} toolchain not held ({e}); an update during this run can \
                 replace {}",
                dir.display()
            ),
        }
    }
    let said = (!held.is_empty()).then(|| {
        let each: Vec<String> = held.iter().map(|l| l.subject().describe()).collect();
        format!(
            "{} held for this run; an unattended trust update waits for {}",
            each.join(", "),
            if held.len() == 1 { "it" } else { "them" }
        )
    });
    (held, said)
}

fn held_dirs(tools: &Toolchain) -> Vec<PathBuf> {
    let mut dirs = vec![tools.stage2_dir.clone()];
    for (_, file) in &tools.borrowed {
        if let Some(dir) = file.parent()
            && !dirs.iter().any(|d| d == dir)
        {
            dirs.push(dir.to_path_buf());
        }
    }
    dirs
}

const LOGS_KEPT: usize = 20;

fn open_log(chosen: Option<&Path>, cwd: &Path, ctx: &Ctx) -> Option<(PathBuf, std::fs::File)> {
    let path = match chosen {
        Some(p) => cwd.join(p),
        None => {
            let dir = ctx.state_dir().join("logs.noindex");
            prune_logs(&dir);
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            dir.join(format!("{secs}-{}.log", std::process::id()))
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::File::create(&path) {
        Ok(f) => Some((path, f)),
        Err(e) => {
            eprintln!("verify: cannot write the run log {}: {e}", path.display());
            None
        }
    }
}

fn prune_logs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if logs.len() < LOGS_KEPT {
        return;
    }
    logs.sort_unstable();
    let drop_n = logs.len() + 1 - LOGS_KEPT;
    for (_, p) in logs.into_iter().take(drop_n) {
        let _ = std::fs::remove_file(p);
    }
}

/// The checkout `cwd` is in.
#[must_use]
pub fn checkout_root(cwd: &Path) -> Option<PathBuf> {
    let out = changed::git(cwd, &std::env::var_os("PATH").unwrap_or_default())
        .args(["rev-parse", "--show-toplevel"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !top.is_empty()).then(|| PathBuf::from(top))
}

/// The commit the checkout at `root` has checked out, in full.
#[must_use]
pub fn head_commit(root: &Path, path_env: &OsStr) -> Option<String> {
    let out = changed::git(root, path_env)
        .args(["rev-parse", "HEAD"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !head.is_empty()).then_some(head)
}

/// The whole CLI, in-process.
#[must_use]
pub fn run_cli(args: &[String], cwd: &Path) -> i32 {
    // `perf`, `soak`, `glue`, `provision` and the browser lane's verbs
    // (`browser-site`, `browser-drive`) parse their own arguments, and none
    // needs targo to start: a host without Trust drives the browser lane, and
    // `provision` names what provides targo where a row needs it.
    if args.first().is_some_and(|a| a == "perf") {
        return perf::run(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "soak") {
        return soak::run(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "glue") {
        return wasm_glue::run(&args[1..]);
    }
    #[cfg(unix)]
    if args.first().is_some_and(|a| a == "provision") {
        return provision::main(&args[1..], cwd);
    }
    #[cfg(not(unix))]
    if args.first().is_some_and(|a| a == "provision") {
        eprintln!("verify provision: COULD NOT RUN — it provisions macOS and Linux hosts only");
        return exit::COULD_NOT_RUN;
    }
    #[cfg(unix)]
    if args.first().is_some_and(|a| a == "browser-site") {
        return browser::site_main(&args[1..], cwd);
    }
    #[cfg(unix)]
    if args.first().is_some_and(|a| a == "browser-drive") {
        return browser::drive_main(&args[1..]);
    }
    let stop = |code: i32, line: String| {
        eprintln!("{line}");
        code
    };
    let parsed = match cli::parse(args.iter().cloned()) {
        Ok(a) => a,
        Err(e) => return stop(exit::USAGE, e.message()),
    };
    if parsed.help {
        print!("{}", cli::usage());
        return exit::PASS;
    }
    if parsed.list {
        print!("{}", checks::list_table());
        return exit::PASS;
    }
    let could_not_run = |why: String| {
        stop(
            exit::COULD_NOT_RUN,
            format!("verify: COULD NOT RUN — {why}"),
        )
    };
    let measure_artifact = match parsed
        .app
        .as_deref()
        .map(|app| MeasureArtifact::at(&cwd.join(app)))
        .transpose()
    {
        Ok(a) => a,
        Err(why) => return stop(exit::USAGE, format!("verify: {why}")),
    };
    if let Some(note) = cli::ignored_selection(&parsed) {
        eprintln!("verify: {note}");
    }
    let Some(root) = checkout_root(cwd) else {
        return could_not_run(format!(
            "{} is not inside a git checkout. Fix: run it from the aterm checkout",
            cwd.display()
        ));
    };
    if toolchain::pinned_channel(&root).is_none() {
        return could_not_run(format!(
            "{} is missing or names no channel. Fix: restore it (channel = \"trust\")",
            root.join("rust-toolchain.toml").display()
        ));
    }
    let env = EnvSnapshot::capture();
    if let Err(why) = test_workers::admitted_share(&env) {
        return stop(exit::USAGE, format!("verify: {why}; no check was run"));
    }
    let tools = run_toolchain(&env, &root);
    if !tools.have_targo() {
        return could_not_run(tools.missing_targo_label());
    }
    if let Some(d) = &tools.demoted {
        eprintln!("verify: toolchain: {}", d.sentence());
    }
    let scratch = match mktemp_dir("atv") {
        Ok(p) => p,
        Err(e) => return could_not_run(format!("cannot create a scratch directory: {e}")),
    };
    let (_leases, lease_said) = hold_toolchain(&env, &tools, &root);
    #[cfg(unix)]
    exec::group::kill_on_interrupt();
    let runs_dir = aterm_uds::control_socket_dir()
        .map(|d| runs::records_dir(&d, aterm_uds::process::pid_namespace().as_deref()));
    if let (Some(dir), Some(boot)) = (&runs_dir, aterm_uds::process::boot_id()) {
        let me = i32::try_from(std::process::id()).unwrap_or(0);
        runs::end_dead_runs(dir, &boot, me, &runs::LiveKernel, &mut |line| {
            eprintln!("verify: {line}");
        });
    }
    #[cfg(target_os = "macos")]
    if let Some(line) = runs::program_check_line(&runs::LiveKernel, || {
        let me = i32::try_from(std::process::id()).unwrap_or(0);
        let (app, listed) = atpkg::devtools::listed_for_responsible(me);
        (app, listed.word())
    }) {
        eprintln!("verify: {line}");
    }

    let path_env = tools.path_with_stage2_first(&env.path);
    let head_at_start = head_commit(&root, &path_env);
    let (scope, cone) = if !parsed.narrows() || parsed.all {
        (Scope::workspace(), None)
    } else if !parsed.packages.is_empty() {
        (Scope::packages(&parsed.packages), None)
    } else {
        let base = parsed
            .base
            .clone()
            .unwrap_or_else(|| changed::default_base(&root, &path_env));
        let sel = changed::resolve(&root, &tools, &path_env, &base);
        let (scope, said) = changed::cone(&base, &sel);
        (scope, Some(said))
    };
    let wanted = checks::with_pulled(&parsed.checks, parsed.release, |p| scope.includes_crate(p));
    let cells_pulled = checks::pulls_cells(&wanted, parsed.release, !scope.selects_nothing());
    let wanted = if cells_pulled {
        checks::with(&wanted, Check::Cells)
    } else {
        wanted
    };
    let ctx = Ctx::new_with_tools(root, scope, env, scratch.clone(), tools)
        .with_checks(&wanted)
        .with_release(parsed.release)
        .with_fix(parsed.fix)
        .with_cells(parsed.cells.clone())
        .with_cells_pulled(cells_pulled)
        .with_no_windows(parsed.no_windows)
        .with_measure_artifact(measure_artifact)
        .with_child_ceiling(parsed.timeout.unwrap_or(Some(exec::DEFAULT_CHILD_CEILING)))
        .with_test_jobs(parsed.test_jobs)
        .with_test_threads(parsed.test_threads)
        .with_quiet(Some(parsed.admit))
        .with_toolchain_lease(lease_said)
        .with_quiet_tmpdir(&std::env::temp_dir())
        .with_head_at_start(head_at_start);
    let ctx = match runs_dir
        .as_deref()
        .and_then(|dir| runs::Runs::open(&ctx, dir, exec::run))
    {
        Some(record) => ctx.with_runs(record),
        None => ctx,
    };
    #[cfg(unix)]
    for line in plan::retire_old_lanes(&ctx) {
        eprintln!("verify: {line}");
    }
    #[cfg(unix)]
    let disk = match disk::admit(&ctx, parsed.admit) {
        Ok(held) => held,
        Err(why) => {
            if let Some(record) = &ctx.runs {
                record.finish(&runs::LiveKernel);
            }
            std::fs::remove_dir_all(&scratch).ok();
            return could_not_run(why);
        }
    };
    let log = open_log(parsed.log.as_deref(), cwd, &ctx);
    let log_shown = log
        .as_ref()
        .map(|(path, _)| path.strip_prefix(cwd).unwrap_or(path).display().to_string());
    let mut sink = std::io::sink();
    let mut log_file = log.as_ref().and_then(|(_, f)| f.try_clone().ok());
    let log_writer: &mut (dyn Write + Send) = match log_file.as_mut() {
        Some(f) => f,
        None => &mut sink,
    };
    let mut out = std::io::stdout();
    let show = Show {
        cone: cone.as_deref(),
        log: log_shown.as_deref(),
    };
    let code = match run(&ctx, &mut out, log_writer, &show) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("verify: cannot write the result: {e}");
            exit::COULD_NOT_RUN
        }
    };
    #[cfg(unix)]
    if let Some(held) = disk {
        let claimed = held.estimate;
        let took = held.finish();
        let line = format!(
            "disk: claimed {claimed}; this run grew its directories by {} (they held {} at most)",
            atpkg::admit_disk::size(took.grew),
            atpkg::admit_disk::size(took.peak)
        );
        let _ = log_file.as_mut().map(|f| writeln!(f, "{line}"));
    }
    if let Some(record) = &ctx.runs {
        record.finish(&runs::LiveKernel);
    }
    std::fs::remove_dir_all(&scratch).ok();
    code
}

/// `mktemp -d`: creates the private directory atomically, and `/tmp` keeps unix-socket paths under macOS's 104-byte `sockaddr_un` limit.
///
/// # Errors
/// Fails when `mktemp` is absent or refuses to create the directory.
pub fn mktemp_dir(tag: &str) -> std::io::Result<PathBuf> {
    let out = Command::new(aterm_uds::os_path!("mktemp"))
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

/// Put an executable `#!/bin/sh` stand-in running `body` at `path`, replacing whatever is there.
///
/// # Panics
/// When `path`'s directory cannot be made or the stand-in cannot be written.
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
    std::fs::write(path, &text).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// [`standin`] for a body that varies.
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
    let mut tmp = data.as_os_str().to_owned();
    tmp.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, format!("{body}\n")).expect("write the stand-in's body");
    std::fs::rename(&tmp, &data).expect("rename the stand-in's body into place");
    standin(path, r#". "$0.body""#);
}

/// `command -v <name>` against the PATH the run hands its children.
#[must_use]
pub fn have_on_path(name: &str, path: &OsStr) -> bool {
    find_on_path(name.as_ref(), path).is_some()
}

/// The file a PATH lookup of `name` runs.
#[must_use]
pub fn find_on_path(name: &OsStr, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|f| is_executable_file(f))
}

/// aterm's directories in this user's cache ([`aterm_uds::host::user_cache`]) on [`host_user_cache`].
pub use aterm_uds::host::{host_user_cache, user_cache};

#[cfg(all(test, unix))]
mod quiet_interrupt_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_borrowed_tools_build_is_held_for_the_run() {
        use aterm_toolchain::lease::{self, Holders, Subject};
        let home = std::fs::canonicalize(mktemp_dir("atv-lease").expect("mktemp")).expect("real");
        let env = EnvSnapshot {
            home: home.clone(),
            xdg_config_home: Some(home.join("xdg")),
            ..EnvSnapshot::default()
        };
        let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
        let managed = prefix.join("store/trust/9192/bin");
        std::fs::create_dir_all(&managed).expect("store build");
        let own = home.join("trust/build/host/stage2/bin");
        let tools = Toolchain {
            stage2_dir: own.clone(),
            targo: own.join("targo"),
            trustdoc: own.join("trustdoc"),
            tippy: Some(managed.join(toolchain::TIPPY)),
            trustfmt: Some(managed.join(toolchain::TRUSTFMT)),
            borrowed: vec![
                (toolchain::TRUSTFMT, managed.join(toolchain::TRUSTFMT)),
                (toolchain::TIPPY, managed.join(toolchain::TIPPY)),
            ],
            refused: None,
            store_bin: Some(prefix.join("store/trust/current/bin")),
            demoted: None,
        };
        let subject = Subject::of_dir(&prefix, &managed).expect("a store build");
        let (held, said) = hold_toolchain(&env, &tools, Path::new("/repo"));
        assert_eq!(held.len(), 1, "one build, held once");
        assert_eq!(held[0].subject(), &subject);
        assert!(matches!(
            lease::holders(&prefix, &subject),
            Holders::Held(_)
        ));
        let said = said.expect("said in the log");
        assert!(said.contains("trust build 9192"), "{said}");
        drop(held);
        assert_eq!(lease::holders(&prefix, &subject), Holders::Free);

        let own_only = Toolchain {
            borrowed: Vec::new(),
            ..tools
        };
        let (none, said) = hold_toolchain(&env, &own_only, Path::new("/repo"));
        assert!(none.is_empty(), "an unmanaged directory takes no lease");
        assert_eq!(said, None);
        assert_eq!(lease::holders(&prefix, &subject), Holders::Free);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let runnable = |path: &Path, body: &str| {
                std::fs::create_dir_all(path.parent().expect("bin")).expect("mkdir");
                std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write");
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod");
                aterm_uds::spawnfd::command(path)
                    .output()
                    .expect("a stand-in just laid runs");
            };
            for bin in [&own, &managed] {
                std::fs::create_dir_all(bin.parent().expect("sysroot").join("lib/rustlib"))
                    .expect("rustlib");
            }
            runnable(&own.join("targo"), "exit 0");
            runnable(
                &own.join("trustc"),
                &format!("echo '{}'", own.parent().expect("sysroot").display()),
            );
            runnable(&managed.join(toolchain::TIPPY), "exit 0");
            let rustup = home.join("rustup");
            std::fs::create_dir_all(rustup.join("toolchains")).expect("toolchains");
            std::os::unix::fs::symlink(
                own.parent().expect("sysroot"),
                rustup.join("toolchains/trust"),
            )
            .expect("rustup entry");
            let shims = prefix.join("bin");
            runnable(&shims.join(toolchain::TIPPY), "exit 0");
            runnable(
                &shims.join("trustc"),
                &format!("echo '{}'", managed.parent().expect("sysroot").display()),
            );
            let discovered = Toolchain::discover_with_store(
                None,
                &rustup,
                Some(&prefix),
                shims.as_os_str(),
                Some("trust"),
            );
            let (held, _) = hold_toolchain(&env, &discovered, Path::new("/repo"));
            assert!(
                matches!(lease::holders(&prefix, &subject), Holders::Held(_)),
                "BORROWED_SHIM_MUST_HOLD_THE_PHYSICAL_BUILD"
            );
            assert_eq!(held.len(), 1);
            assert_eq!(discovered.tippy, Some(managed.join(toolchain::TIPPY)));
            drop(held);
            assert_eq!(lease::holders(&prefix, &subject), Holders::Free);
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn every_child_builds_in_the_callers_target_dir_and_gets_the_threads_asked_for() {
        let theirs = Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot {
                cargo_target_dir: Some(PathBuf::from("/fast/tgt")),
                ..EnvSnapshot::default()
            },
            PathBuf::from("/tmp"),
        );
        let has = |c: &Ctx, k: &str, v: &str| {
            c.exec_env()
                .add_env
                .iter()
                .any(|(ek, ev)| ek == k && ev == v)
        };
        assert!(has(&theirs, "CARGO_TARGET_DIR", "/fast/tgt"));
        assert!(
            !theirs
                .child_env_add
                .iter()
                .any(|(k, _)| k == "RUST_TEST_THREADS")
        );
        let env = theirs.exec_env();
        assert_eq!(env.remove_env, CHILD_ENV_REMOVED);
        for k in ["CARGO_MANIFEST_DIR", "CARGO_PKG_*", "CARGO_PRIMARY_PACKAGE"] {
            assert!(env.remove_env.contains(&k), "{k}");
        }
        let threaded = theirs.with_test_threads(std::num::NonZeroU32::new(3));
        assert!(has(&threaded, "RUST_TEST_THREADS", "3"));
    }

    /// A run's children get the temp directory's quiet folder as `TMPDIR` on macOS.
    /// NEGATIVE CONTROL: a temp directory quiet already, and any off macOS, is left as it is.
    #[test]
    fn stage_children_get_a_quiet_tmpdir() {
        let ctx = || {
            Ctx::new(
                PathBuf::from("/repo"),
                Scope::workspace(),
                EnvSnapshot::default(),
                PathBuf::from("/tmp"),
            )
        };
        let tmpdir = |c: &Ctx| {
            c.child_env_add
                .iter()
                .find(|(k, _)| k == "TMPDIR")
                .map(|(_, v)| PathBuf::from(v))
        };
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        assert_eq!(tmpdir(&ctx().with_quiet_tmpdir(scratch.path())), None);
        let loud = PathBuf::from("/tmp");
        let got = tmpdir(&ctx().with_quiet_tmpdir(&loud));
        if cfg!(target_os = "macos") {
            let got = got.expect("a quiet TMPDIR");
            assert_eq!(got, loud.join(".ubd"));
            assert!(got.as_os_str().to_string_lossy().ends_with("/.ubd/"));
            assert!(aterm_tempfile::is_quiet(scratch.path()), "{scratch:?}");
        } else {
            assert_eq!(got, None);
        }
    }

    #[test]
    fn the_main_lane_shares_plain_targos_cache_exactly_where_the_shim_would() {
        let tmp = std::fs::canonicalize(mktemp_dir("atv-cache").expect("mktemp")).expect("real");
        let home = tmp.join("home");
        let root = home.join("Developer/repo");
        std::fs::create_dir_all(home.join("Library/Caches")).expect("mkdir");
        std::fs::create_dir_all(&root).expect("mkdir");
        let env = EnvSnapshot {
            home: home.clone(),
            ..EnvSnapshot::default()
        };
        let got = main_build_dir(&env, &root);
        if cfg!(target_os = "macos") {
            assert_eq!(
                got,
                Some(format!(
                    "build.build-dir='{}/Library/Caches/aterm/.ubd/cargo-build.noindex\
                     {{workspace-root}}/.cargo-build'",
                    home.display()
                ))
            );
            let c = Ctx::new(
                root.clone(),
                Scope::workspace(),
                env.clone(),
                PathBuf::from("/tmp"),
            );
            assert_eq!(c.main_build_dir, got);
            assert!(!c.child_env_add.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"));
            assert_eq!(c.target_dir, root.join("target"));
            for var in &CARGO_LAYOUT_VARS[..3] {
                let chosen = EnvSnapshot {
                    cargo_layout: vec![(*var, OsString::new())],
                    ..env.clone()
                };
                assert_eq!(main_build_dir(&chosen, &root), None, "{var}");
            }
            assert_eq!(
                main_build_dir(&env, &tmp.join("elsewhere")),
                None,
                "outside HOME"
            );
            // A shim laid before the cache moved under .ubd names the old root: the main lane
            // builds there with it. NEGATIVE CONTROL: this version's shim names the quiet one.
            let bin = tmp.join("bin");
            std::fs::create_dir_all(&bin).expect("mkdir");
            let on_path = EnvSnapshot {
                path: bin.clone().into_os_string(),
                ..env.clone()
            };
            for (under, quiet) in [
                (aterm_toolchain::build_cache::MOVED_FROM_UNDER_HOME, false),
                (aterm_toolchain::build_cache::CACHE_UNDER_HOME, true),
            ] {
                std::fs::write(
                    bin.join("targo"),
                    format!("#!/bin/sh\n# atpkg shim\n__atb_t=\n__atb_c=${{HOME-}}/{under}\n"),
                )
                .expect("a shim");
                let value = main_build_dir(&on_path, &root).expect("the shim's cache");
                assert_eq!(
                    value,
                    format!(
                        "build.build-dir='{}/{under}{{workspace-root}}/.cargo-build'",
                        home.display()
                    )
                );
                assert_eq!(value.contains("/.ubd/"), quiet, "{value}");
            }
        } else {
            assert_eq!(got, None, "the shim's cache is macOS only");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_pinned_main_lane_keeps_its_intermediates_out_of_spotlight() {
        let pinned = |c: &Ctx| {
            c.child_env_add
                .iter()
                .find(|(k, _)| k == "CARGO_TARGET_DIR")
                .map(|(_, v)| PathBuf::from(v))
        };
        let ctx = |root: &Path, env: EnvSnapshot| {
            Ctx::new(
                root.to_path_buf(),
                Scope::workspace(),
                env,
                PathBuf::from("/tmp"),
            )
        };
        let theirs = EnvSnapshot {
            cargo_target_dir: Some(PathBuf::from("/fast/tgt")),
            cargo_layout: vec![("CARGO_TARGET_DIR", "/fast/tgt".into())],
            ..EnvSnapshot::default()
        };
        let c = ctx(Path::new("/repo"), theirs.clone());
        assert_eq!(pinned(&c), Some(PathBuf::from("/fast/tgt")));
        assert_eq!(
            c.main_build_dir.as_deref(),
            Some("build.build-dir='/fast/tgt/build.noindex'")
        );
        let argv = stages::main_targo(&c, vec!["--unverified".into(), "test".into()]).argv();
        assert_eq!(
            argv[1..],
            [
                "--config",
                "build.build-dir='/fast/tgt/build.noindex'",
                "--unverified",
                "test"
            ]
        );
        let c = ctx(Path::new("/repo"), EnvSnapshot::default());
        assert_eq!(pinned(&c), Some(PathBuf::from("/repo/target")));
        assert_eq!(
            c.main_build_dir.as_deref(),
            Some("build.build-dir='/repo/target/build.noindex'")
        );
        let chose = EnvSnapshot {
            cargo_layout: vec![("CARGO_BUILD_BUILD_DIR", OsString::new())],
            ..theirs.clone()
        };
        let c = ctx(Path::new("/repo"), chose);
        assert_eq!((pinned(&c).is_some(), c.main_build_dir), (true, None));
        let tmp = std::fs::canonicalize(mktemp_dir("atv-bdir").expect("mktemp")).expect("real");
        let root = tmp.join("repo");
        std::fs::create_dir_all(root.join(".cargo")).expect("mkdir");
        std::fs::write(
            root.join(".cargo/config.toml"),
            "[build]\nbuild-dir = 'mine'\n",
        )
        .expect("config");
        let c = ctx(&root, theirs);
        assert_eq!((pinned(&c).is_some(), c.main_build_dir), (true, None));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_callers_rust_test_threads_pins_every_binary() {
        let c = Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot {
                rust_test_threads: Some("5".into()),
                ..EnvSnapshot::default()
            },
            PathBuf::from("/tmp"),
        );
        assert_eq!(c.test_threads, Some(5));
        assert_eq!(c.test_threads(), 5);
        let unpinned = Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        );
        assert_eq!(unpinned.test_threads, None);
    }

    #[test]
    fn a_rerun_keeps_no_windows_where_the_flag_changes_the_check() {
        let c = |no_windows| {
            Ctx::new(
                PathBuf::from("/repo"),
                Scope::workspace(),
                EnvSnapshot::default(),
                PathBuf::from("/tmp"),
            )
            .with_no_windows(no_windows)
        };
        let (quiet, plain) = (c(true), c(false));
        for check in [Check::Browser, Check::Appkit] {
            assert!(quiet.rerun(check).ends_with(" --no-windows"), "{check:?}");
            assert!(!plain.rerun(check).contains("--no-windows"), "{check:?}");
        }
        assert!(!quiet.rerun(Check::Fmt).contains("--no-windows"));
    }

    /// A bundle names its version and commit from its `Info.plist`, and HEAD built it only when
    /// that commit, less `-dirty`, begins HEAD.
    #[cfg(unix)]
    #[test]
    fn a_measured_bundle_names_the_commit_that_built_it() {
        let tmp = mktemp_dir("atv-app").expect("mktemp");
        let app = tmp.join("aterm.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).expect("mkdir");
        let bin = app.join("Contents/MacOS/aterm");
        std::fs::write(&bin, "#!/bin/sh\n").expect("write");
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        let unnamed = MeasureArtifact::at(&app).expect("a bundle");
        assert_eq!(
            (unnamed.version.as_deref(), unnamed.commit.as_deref()),
            (None, None)
        );
        assert!(
            !unnamed.built_from("22cb722a2ec9ab"),
            "no commit is no build of HEAD"
        );
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<dict>\n\t<key>ATermGitCommit</key>\n\t<string>22cb722a2ec9</string>\n\
             \t<key>CFBundleShortVersionString</key>\n\t<string>0.137.0</string>\n</dict>\n",
        )
        .expect("write");
        let named = MeasureArtifact::at(&app).expect("a bundle");
        assert_eq!(named.version.as_deref(), Some("0.137.0"));
        assert_eq!(named.commit.as_deref(), Some("22cb722a2ec9"));
        assert!(
            named
                .describe()
                .starts_with("aterm 0.137.0 (22cb722a2ec9) at ")
        );
        assert!(named.built_from("22cb722a2ec9abcdef0123456789abcdef012345"));
        assert!(!named.built_from("215fb53331abd9acb2a33e7d7a21d1fa3c2bd8f2"));
        let dirty = MeasureArtifact {
            commit: Some("22cb722a2ec9-dirty".into()),
            ..named
        };
        assert!(dirty.built_from("22cb722a2ec9abcdef0123456789abcdef012345"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A usage error is 2; no checkout or no pin is 3 with its fix, before anything runs.
    #[cfg(unix)]
    #[test]
    fn the_cli_refuses_usage_and_a_place_it_cannot_run_from() {
        let args = |a: &[&str]| a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let tmp = mktemp_dir("atv-cli").expect("mktemp");
        assert_eq!(run_cli(&args(&["--fast"]), &tmp), exit::USAGE);
        assert_eq!(run_cli(&args(&["release"]), &tmp), exit::USAGE);
        assert_eq!(
            run_cli(&args(&["measure", "--app", "/nonexistent/aterm.app"]), &tmp),
            exit::USAGE
        );
        assert_eq!(
            run_cli(&args(&["fmt"]), &tmp),
            exit::COULD_NOT_RUN,
            "no checkout"
        );
        let git = std::process::Command::new(aterm_uds::os_path!("git"))
            .args(["init", "-q"])
            .current_dir(&tmp)
            .status()
            .expect("git");
        assert!(git.success());
        assert_eq!(
            run_cli(&args(&["fmt"]), &tmp),
            exit::COULD_NOT_RUN,
            "no rust-toolchain.toml"
        );
        assert_eq!(run_cli(&args(&["list"]), &tmp), exit::PASS);
        assert_eq!(run_cli(&args(&["--help"]), &tmp), exit::PASS);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn the_header_names_what_the_selection_pulled_and_why() {
        let head = |crates: &[&str]| {
            let scope = Scope::changed(
                "origin/main",
                crates.iter().map(|c| c.to_string()).collect(),
                true,
            );
            let holds = |p: &str| scope.includes_crate(p);
            let checks = checks::with_pulled(&checks::DEV, false, holds);
            let ctx = Ctx::new(
                PathBuf::from("/repo"),
                scope.clone(),
                EnvSnapshot::default(),
                PathBuf::from("/tmp"),
            )
            .with_checks(&checks);
            header(&ctx, Some("the cone"))
        };
        assert_eq!(head(&["aterm-gui"]), "verify: dev — the cone");
        assert_eq!(
            head(&["aterm-gpu"]),
            "verify: dev + gpu-oracle (the selection holds aterm-gpu) — the cone"
        );
        assert_eq!(
            head(&["aterm-core", "aterm-gpu", "aterm-gpu-web", "aterm-wasm"]),
            "verify: dev + gpu-oracle + wasm-smoke (the selection holds aterm-gpu, aterm-wasm) \
             — the cone"
        );
        let pulled = |crates: &[&str]| {
            let scope = Scope::changed(
                "origin/main",
                crates.iter().map(|c| c.to_string()).collect(),
                true,
            );
            let holds = |p: &str| scope.includes_crate(p);
            let checks = checks::with(
                &checks::with_pulled(&checks::DEV, false, holds),
                Check::Cells,
            );
            let ctx = Ctx::new(
                PathBuf::from("/repo"),
                scope.clone(),
                EnvSnapshot::default(),
                PathBuf::from("/tmp"),
            )
            .with_checks(&checks)
            .with_cells_pulled(true);
            header(&ctx, Some("the cone"))
        };
        assert_eq!(
            pulled(&["aterm-gui"]),
            "verify: dev + cells (each other OS's cell that builds a selected crate) — the cone"
        );
        assert_eq!(
            pulled(&["aterm-gpu"]),
            "verify: dev + gpu-oracle (the selection holds aterm-gpu) + cells (each other OS's \
             cell that builds a selected crate) — the cone"
        );
    }

    /// A stage that commits in the run's checkout, standing in for another session's
    /// fast-forward of a shared checkout mid-run.
    fn committing_stage(ctx: &Ctx, spec: &plan::StageSpec) -> Report {
        let out = changed::git(&ctx.root, &ctx.path_env)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["commit", "-q", "--allow-empty", "-m", "moved"])
            .output()
            .expect("git runs");
        assert!(out.status.success(), "commit: {out:?}");
        let mut r = Report::new(spec.title.clone());
        r.pass("ran");
        r
    }

    fn passing_stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
        let mut r = Report::new(spec.title.clone());
        r.pass("ran");
        r
    }

    /// A run whose checkout's HEAD moves while its stages run ends COULD NOT RUN, its checks'
    /// own PASS beside it. Negative control: the same run with HEAD still ends PASS.
    #[test]
    fn a_run_whose_checkout_moved_under_its_stages_could_not_run() {
        let root = mktemp_dir("atv-moved-checkout").expect("mktemp");
        let path_env = std::env::var_os("PATH").unwrap_or_default();
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
        ] {
            let out = changed::git(&root, &path_env)
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        }
        let run = |stage: fn(&Ctx, &plan::StageSpec) -> Report| {
            let mut ctx = Ctx::new(
                root.clone(),
                Scope::crate_only("aterm-verify"),
                EnvSnapshot::default(),
                root.join("scratch"),
            )
            .with_checks(&[Check::Fmt]);
            ctx.path_env.clone_from(&path_env);
            let mut out = Vec::new();
            let code = run_with(
                &ctx,
                &mut out,
                &mut std::io::sink(),
                &Show::default(),
                Seams {
                    stage,
                    admit_quiet: LIVE.admit_quiet,
                },
            )
            .expect("the result written");
            (code, String::from_utf8_lossy(&out).into_owned())
        };

        let (code, out) = run(passing_stage);
        assert_eq!(code, exit::PASS, "HEAD never moved: {out}");

        let (code, out) = run(committing_stage);
        assert_eq!(code, exit::COULD_NOT_RUN, "{out}");
        assert!(out.contains("the checkout moved from"), "{out}");
        std::fs::remove_dir_all(&root).ok();
    }
}
