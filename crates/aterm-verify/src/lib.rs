// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-verify` — every aterm check as one CLI: `targo --unverified verify [CHECK|SET ...]`.
//!
//! It runs in place, in the caller's checkout and target dir, offline, and
//! takes no lock but a short machine-wide one around the checks that judge
//! timings. A bare run is the `dev` set on the change cone. Each check's
//! stages run concurrently where they share no resource ([`sched`]), and the
//! run prints one line per check as it finishes ([`report`]), the full ladder
//! going to a log. `verify perf` ([`perf`]), `verify soak` ([`soak`]) and `verify glue`
//! ([`wasm_glue`]) are verbs beside the checks, with arguments of their own. `ship release`
//! runs `verify release --app <bundle>` and publishes only on `0`.
//!
//! Exit codes ([`exit`]): `0` every check that ran passed, `1` a check failed,
//! `2` usage, `3` could not run. A skip names what was absent and its fix and
//! fails nothing, except under `release`.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the verify CLI spawns its checks"
)]

// The browser lane drives Chromium's DevTools over two inherited pipes, a
// unix spawn (`aterm_uds::spawnfd::spawn_with_two_fds`).
#[cfg(unix)]
pub mod browser;
pub mod cells;
pub mod changed;
pub mod checks;
pub mod cli;
pub mod docscan;
pub mod exec;
pub mod findings;
pub mod fmt;
pub mod ladder;
pub mod libtest;
pub mod perf;
pub mod plan;
pub mod report;
pub mod rustup;
pub mod sched;
pub mod scope;
// The GUI smoke drives a unix control socket with unix process plumbing; off
// unix its row is a named skip (`stages::run_stage`).
#[cfg(unix)]
pub mod smoke;
#[cfg(unix)]
pub mod smoke_stages;
pub mod soak;
pub mod stages;
pub mod testrun;
/// THE toolchain resolution, shared with atpkg (`aterm help rust`), the libc
/// oracle's gate and the release cutter: the std-only `aterm-toolchain`.
pub use aterm_toolchain::discovery as toolchain;
pub mod wasm_glue;
pub mod wasm_smoke;

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use checks::Check;
pub use ladder::{Outcome, Report, Severity};
pub use scope::Scope;
pub use toolchain::{Toolchain, is_executable_file};

/// Exit codes. FAILED and COULD-NOT-RUN stay apart: a broken environment is
/// not a finding about the tree.
pub mod exit {
    /// Every check that ran passed.
    pub const PASS: i32 = 0;
    /// A check FAILED — a real finding about the tree.
    pub const FAILED: i32 = 1;
    /// Usage error.
    pub const USAGE: i32 = 2;
    /// COULD NOT RUN — nothing was decided; the output says why.
    pub const COULD_NOT_RUN: i32 = 3;
}

/// The environment, read once at startup on the main thread, so every
/// concurrent stage decides from a value a test can construct.
#[derive(Clone, Debug, Default)]
pub struct EnvSnapshot {
    pub path: OsString,
    pub home: PathBuf,
    pub trust_stage2_bin: Option<PathBuf>,
    /// `XDG_CONFIG_HOME`: where atpkg's config lives ([`toolchain::atpkg_prefix`]).
    pub xdg_config_home: Option<PathBuf>,
    /// `RUSTUP_HOME`, raw ([`toolchain::rustup_home`]).
    pub rustup_home: Option<OsString>,
    /// `SSH_CONNECTION`: a remote session has no WindowServer to drive.
    pub ssh_connection: Option<String>,
    /// `CARGO_BUILD_JOBS`, raw: a ceiling on every side lane's job count.
    pub cargo_build_jobs: Option<OsString>,
    /// `RUSTDOC` or `CARGO_BUILD_RUSTDOC`: the caller's doc-driver override,
    /// which cargo prefers over the config's `[build] rustdoc`.
    pub rustdoc_override: Option<OsString>,
    /// `CARGO_TARGET_DIR`, absolute: the caller's target dir, which every run
    /// builds in.
    pub cargo_target_dir: Option<PathBuf>,
    /// The variables that choose cargo's layout, raw and only when set
    /// ([`CARGO_LAYOUT_VARS`]): what decides whether the main lane gets plain
    /// `targo`'s build cache ([`main_build_dir`]).
    pub cargo_layout: Vec<(&'static str, OsString)>,
    /// `RUST_TEST_THREADS`, raw: a caller's pin, which the run never overrides.
    pub rust_test_threads: Option<OsString>,
    /// `CARGO_HOME`: where rustup-init put `rustup` ([`rustup::find_rustup_in`]).
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
            // cargo treats a set-but-empty RUSTDOC as set, so it still masks
            // CARGO_BUILD_RUSTDOC.
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

/// The variables that move cargo's target or build directory, and the home whose
/// config may: any one set (even empty) and atpkg's shim gives plain `targo` no
/// shared cache, so the run does not either ([`main_build_dir`]).
pub const CARGO_LAYOUT_VARS: [&str; 4] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_BUILD_DIR",
    "CARGO_HOME",
];

/// The `build.build-dir` value atpkg's `targo` shim hands a build at `root` when
/// it gives the shared cache (`aterm_toolchain::build_cache::decide`, the shim's
/// own rule), so the main lane's children share plain `targo`'s cache: `None` off
/// macOS, and wherever the shim would not give it (the caller chose a target or
/// build directory, a cargo config names one, the root is not under `$HOME`, the
/// cache cannot be written).
#[must_use]
pub fn main_build_dir(env: &EnvSnapshot, root: &Path) -> Option<String> {
    use aterm_toolchain::build_cache::{BuildDir, Machine, Tool, decide, read_config};
    if !cfg!(target_os = "macos") {
        return None;
    }
    let lookup = |k: &str| {
        env.cargo_layout
            .iter()
            .find(|(name, _)| *name == k)
            .map(|(_, v)| v.clone())
    };
    let machine = Machine {
        home: Some(&env.home),
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

/// The `build.build-dir` the main lane's children get where atpkg's shim would
/// leave the layout alone ([`main_build_dir`] is `None`) and the run pins them to
/// `target` (`CARGO_TARGET_DIR`): `<target>/build.noindex`, so the intermediates
/// (deps, incremental, fingerprints, build scripts) stay out of Spotlight and
/// only the uplifted artifacts (`<target>/debug/aterm`) land in the plain
/// folder (`aterm_toolchain::build_cache::target_build_dir`). `None` where the
/// caller chose a build directory (`CARGO_BUILD_BUILD_DIR`, a cargo config's
/// `build-dir`) or `target` is a path the literal cannot hold.
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
    // SAFETY: `c` is a NUL-terminated string that outlives the call, and
    // `access` only reads it.
    Some(unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
}

/// Off unix the shim's cache is never chosen ([`main_build_dir`] is macOS only).
#[cfg(not(unix))]
fn writable_dir(_dir: &Path) -> Option<bool> {
    None
}

/// The release bundle `measure` judges (`--app <APP>`): the bytes publication
/// signs, never an `aterm` the run builds itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureArtifact {
    /// The bundle, canonical: `<…>/aterm.app`.
    pub app: PathBuf,
}

impl MeasureArtifact {
    /// The bundle at `app`: a directory holding an executable
    /// `Contents/MacOS/aterm`.
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
        Ok(Self { app })
    }

    /// `<app>/Contents/MacOS/<name>`: `aterm`, or one of the argv0 names the
    /// bundle links to it (`aterm-gui`, `aterm-ctl`).
    #[must_use]
    pub fn bin(&self, name: &str) -> PathBuf {
        self.app.join("Contents/MacOS").join(name)
    }
}

/// Everything a stage needs, immutable for the whole run so stages share it
/// across threads without a lock.
#[derive(Debug)]
pub struct Ctx {
    pub root: PathBuf,
    /// The caller's `CARGO_TARGET_DIR`, else `<root>/target`; the side lanes
    /// live under its `verify/` ([`plan::lane_dir`]).
    pub target_dir: PathBuf,
    /// The checks this run runs, in [`checks::CHECKS`] order.
    pub checks: Vec<Check>,
    /// The `release` set: the lint's required-features and shipped-build passes
    /// run, and a skip is exit 3.
    pub release: bool,
    /// `--fix`: `fmt` rewrites the files it finds unformatted.
    pub fix: bool,
    /// `--cell`: the forge cells `cells` checks; empty is the foreign ones.
    pub cells: Vec<String>,
    pub scope: Scope,
    pub tools: Toolchain,
    /// PATH for every child: the resolved stage2 directory first.
    pub path_env: OsString,
    /// Private scratch directory for captured child output.
    pub scratch: PathBuf,
    pub env: EnvSnapshot,
    /// Variables removed from every child's environment ([`CHILD_ENV_REMOVED`]).
    pub child_env_remove: Vec<&'static str>,
    /// Variables given to every child, after the removals and before a stage's
    /// own ([`exec::ExecEnv::add_env`]).
    pub child_env_add: Vec<(OsString, OsString)>,
    /// The `build.build-dir` the main lane's children are handed as `--config`:
    /// plain `targo`'s cache where atpkg's shim would hand one ([`main_build_dir`]),
    /// and no `CARGO_TARGET_DIR`; anywhere else the children are pinned to
    /// [`Ctx::target_dir`] and this is `<target>/build.noindex`
    /// ([`fallback_build_dir`]), `None` only where the caller chose a build
    /// directory.
    pub main_build_dir: Option<String>,
    /// What the run's toolchain lease is, for the log ([`hold_toolchain`]).
    pub toolchain_lease: Option<String>,
    /// The wall-clock ceiling on one stage child; `None` for `--timeout off`.
    pub child_ceiling: Option<Duration>,
    /// `--no-windows`: every stage that opens a window ([`plan::WINDOWED`])
    /// answers a named skip, and a stage that opens one in some rows
    /// ([`plan::WINDOWED_ROWS`]) skips those rows by name.
    pub no_windows: bool,
    /// `--app <APP>`: the bundle `measure` judges.
    pub measure_artifact: Option<MeasureArtifact>,
    /// `--test-jobs`: the most test binaries the test stage runs at once.
    pub test_jobs: u32,
    /// `--test-threads`, else the caller's own `RUST_TEST_THREADS`: the threads
    /// every test binary gets. `None` gives each the machine's parallelism
    /// ([`Ctx::test_threads`]).
    pub test_threads: Option<u32>,
    /// The program cargo runs in place of each test binary to record it
    /// ([`testrun::RECORD_FLAG`]): this very binary unless a test names one.
    pub test_recorder: Option<PathBuf>,
    /// The machine-wide lock the timing stages hold while they run
    /// ([`plan::TIMED`]); `None` takes none.
    pub timing_lock: Option<PathBuf>,
}

/// A `CARGO_BUILD_JOBS` value as a job count: a positive integer, surrounding
/// whitespace allowed; anything else is `None`.
#[must_use]
pub fn parse_build_jobs(raw: Option<&OsStr>) -> Option<u32> {
    raw.and_then(OsStr::to_str)
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| n > 0)
}

/// What `cargo run` hands the verify process about ITSELF (the `verify` alias is
/// `run -q -p aterm-verify`). A child build inheriting them sees them as its own
/// environment: ring's build script tracks `CARGO_MANIFEST_DIR`, so 18 packages up
/// to aterm-gui rebuilt at every switch between a run and a manual build. A name
/// ending in `*` is a prefix ([`exec::ExecEnv::remove_env`]).
pub const CARGO_RUN_ENV: [&str; 6] = [
    "CARGO_MANIFEST_DIR",
    "CARGO_MANIFEST_PATH",
    "CARGO_PKG_*",
    "CARGO_CRATE_NAME",
    "CARGO_BIN_NAME",
    "CARGO_PRIMARY_PACKAGE",
];

/// Variables removed from every child's inherited environment: what `cargo run`
/// said about this process.
pub const CHILD_ENV_REMOVED: [&str; 6] = CARGO_RUN_ENV;

/// The run's toolchain, discovered once from `env` under the pin `root`
/// declares and the atpkg prefix atpkg itself would read.
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

/// Where the machine-wide timing lock lives: one per user, outside every
/// checkout, so two checkouts' timing checks never overlap.
#[must_use]
pub fn default_timing_lock() -> PathBuf {
    std::env::temp_dir().join("aterm-verify-timing.lock")
}

impl Ctx {
    /// The run context, discovering its toolchain ([`run_toolchain`]). It runs
    /// the `dev` set until [`Ctx::with_checks`] says otherwise.
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
        // ONE CACHE WITH PLAIN TARGO. Where atpkg's shim would hand a build its
        // build-cache `--config`, the main lane's children get the same one and
        // no CARGO_TARGET_DIR, which would take them off the shared cache and
        // fork a second one; the target dir is `<root>/target` either way. Anywhere
        // else (a caller's CARGO_TARGET_DIR among them) every child is pinned to
        // the run's target dir.
        // Pinned there, the intermediates still go where Spotlight never looks:
        // `<target>/build.noindex`, unless the caller chose a build directory
        // ([`fallback_build_dir`]).
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
        // Every child runs this run's compiler rather than re-discovering one.
        if tools.have_targo() {
            child_env_add.push((
                "TRUST_STAGE2_BIN".into(),
                tools.stage2_dir.clone().into_os_string(),
            ));
        }
        // A caller's own RUST_TEST_THREADS is a pin, as --test-threads is.
        let pinned_threads = parse_build_jobs(env.rust_test_threads.as_deref());
        Self {
            root,
            target_dir,
            checks: checks::DEV.to_vec(),
            release: false,
            fix: false,
            cells: Vec::new(),
            scope,
            tools,
            path_env,
            scratch,
            env,
            child_env_remove: CHILD_ENV_REMOVED.to_vec(),
            child_env_add,
            main_build_dir,
            toolchain_lease: None,
            child_ceiling: Some(exec::DEFAULT_CHILD_CEILING),
            no_windows: false,
            measure_artifact: None,
            test_jobs: testrun::DEFAULT_JOBS,
            test_threads: pinned_threads,
            test_recorder: None,
            timing_lock: None,
        }
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
    pub fn with_release(mut self, release: bool) -> Self {
        self.release = release;
        self
    }

    /// `--fix` ([`Ctx::fix`]).
    #[must_use]
    pub fn with_fix(mut self, fix: bool) -> Self {
        self.fix = fix;
        self
    }

    /// `--cell` ([`Ctx::cells`]).
    #[must_use]
    pub fn with_cells(mut self, cells: Vec<String>) -> Self {
        self.cells = cells;
        self
    }

    /// The cargo the in-process forge walks ask for graphs: this run's
    /// `targo` under this run's PATH, never `$CARGO` or another `targo` on
    /// PATH.
    #[must_use]
    pub fn forge_cargo(&self) -> aterm_forge::resolve::Cargo {
        aterm_forge::resolve::Cargo::pinned(&self.tools.targo, &self.path_env)
    }

    /// The run's own corner of the target dir (its logs, the foreign cells):
    /// `<target>/verify`.
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

    /// The most compiler jobs a side lane may take: the caller's
    /// `CARGO_BUILD_JOBS` ([`parse_build_jobs`]); `None` leaves the lane's own.
    #[must_use]
    pub fn build_jobs_ceiling(&self) -> Option<u32> {
        parse_build_jobs(self.env.cargo_build_jobs.as_deref())
    }

    /// The most test binaries that run at once ([`Ctx::test_jobs`], at least one).
    #[must_use]
    pub fn test_jobs(&self) -> usize {
        usize::try_from(self.test_jobs.max(1)).unwrap_or(1)
    }

    /// `--test-threads` (or the caller's `RUST_TEST_THREADS`), or the machine's
    /// parallelism, as libtest would pick: what every test binary gets.
    #[must_use]
    pub fn test_threads(&self) -> u32 {
        self.test_threads.unwrap_or_else(|| self.cores())
    }

    /// The recording runner: the named program, else this process's own
    /// executable.
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
        self
    }

    /// `--test-threads <n>`: every test binary gets `RUST_TEST_THREADS=n`.
    #[must_use]
    pub fn with_test_threads(mut self, threads: Option<std::num::NonZeroU32>) -> Self {
        if let Some(n) = threads {
            self.test_threads = Some(n.get());
            self.child_env_add
                .push(("RUST_TEST_THREADS".into(), n.get().to_string().into()));
        }
        self
    }

    /// Move the stage-child ceiling (`--timeout`; `None` is `off`).
    #[must_use]
    pub fn with_child_ceiling(mut self, ceiling: Option<Duration>) -> Self {
        self.child_ceiling = ceiling;
        self
    }

    /// `--no-windows`.
    #[must_use]
    pub fn with_no_windows(mut self, no_windows: bool) -> Self {
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

    /// Take the timing lock at `path` around the timing stages.
    #[must_use]
    pub fn with_timing_lock(mut self, path: Option<PathBuf>) -> Self {
        self.timing_lock = path;
        self
    }

    /// The command environment: cwd is the repo root, PATH the computed one,
    /// and every child gets the wall-clock ceiling.
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

/// The machine-wide timing lock, held: released by `LOCK_UN` before its file
/// closes.
struct TimingLock(std::fs::File);

impl TimingLock {
    /// Take the lock at `path`, waiting for a holder, said once on stderr.
    /// Advisory: a lock that cannot be opened or taken is `None`, and the
    /// stage runs anyway.
    fn take(path: &Path) -> Option<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .ok()?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the take of TimingLock, whose Drop releases by LOCK_UN"
        )]
        let tried = file.try_lock();
        match tried {
            Ok(()) => Some(Self(file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                eprintln!(
                    "verify: waiting for another run's timing check to finish ({})",
                    path.display()
                );
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the take of TimingLock, whose Drop releases by LOCK_UN"
                )]
                let waited = file.lock();
                waited.ok().map(|()| Self(file))
            }
            Err(std::fs::TryLockError::Error(_)) => None,
        }
    }
}

impl Drop for TimingLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Run `f` holding the run's timing lock, when it has one: two runs' timing
/// stages never overlap.
fn with_timing_lock<T>(ctx: &Ctx, f: impl FnOnce() -> T) -> T {
    let _held = ctx.timing_lock.as_deref().and_then(TimingLock::take);
    f()
}

/// The `  time  ` line under a stage in the log: its running time and its
/// start offset.
#[must_use]
pub fn time_line(begun: Duration, ran: Duration) -> String {
    format!(
        "  time  {:.1}s (started +{:.1}s)\n",
        ran.as_secs_f64(),
        begun.as_secs_f64()
    )
}

/// The one header line: what runs, over what ([`selection`]).
#[must_use]
pub fn header(ctx: &Ctx, cone: Option<&str>) -> String {
    let names: Vec<&str> = ctx.checks.iter().map(|c| c.name()).collect();
    let holds = |p: &str| ctx.scope.includes_crate(p);
    let pulled = checks::pulled(&checks::DEV, holds);
    let what = if ctx.release {
        "release".to_string()
    } else if ctx.checks == checks::DEV {
        "dev".to_string()
    } else if !pulled.is_empty() && ctx.checks == checks::with_pulled(&checks::DEV, false, holds) {
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
    format!("verify: {what} — {}", selection(ctx, cone))
}

/// What the run's checks read: the whole tree when none narrows, else the
/// change cone as `cone` says it, the whole workspace, or the `-p` list.
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

/// When a stage started (from the run's start) and how long it ran: its `time`
/// line in the log.
type Clock = (Duration, Duration);

/// One check's progress while the stages run.
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
    /// Where the full log is, as the caller should read it: said after the
    /// verdict.
    pub log: Option<&'a str>,
}

/// Run the selected checks and print their lines to `out` as each finishes,
/// then the verdict line and the log's path; the full ladder goes to `log` in
/// declared order. Returns the exit code.
///
/// # Errors
/// Propagates write failures on `out`.
pub fn run(
    ctx: &Ctx,
    out: &mut (dyn Write + Send),
    log: &mut (dyn Write + Send),
    show: &Show<'_>,
) -> std::io::Result<i32> {
    let cone = show.cone;
    let started = Instant::now();
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
    let mut log_err = None;

    sched::run_stages(
        &stages,
        |spec| {
            let i = stages
                .iter()
                .position(|s| std::ptr::eq(s, spec))
                .unwrap_or_default();
            let owner = owners[i].and_then(|c| ctx.checks.iter().position(|k| *k == c));
            let begun_at = Instant::now();
            if let Some(k) = owner
                && let Ok(mut p) = progress[k].lock()
            {
                p.started.get_or_insert(begun_at);
            }
            let report = if spec.id.timed() {
                with_timing_lock(ctx, || stages::run_stage(ctx, spec))
            } else {
                stages::run_stage(ctx, spec)
            };
            if let Ok(mut c) = clocks.lock() {
                c[i] = Some((begun_at.saturating_duration_since(t0), begun_at.elapsed()));
            }
            // The check's line goes out the moment its last stage is done.
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
            let clock = clocks.lock().ok().and_then(|c| c.get(i).copied().flatten());
            let mut text = report.render();
            if let Some((begun, ran)) = clock {
                text.push_str(&time_line(begun, ran));
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
    let seconds = started.elapsed().as_secs_f64();
    let (line, code) = report::verdict(&results, ctx.release, seconds);
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

/// The toolchain the run resolved, for the log.
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
        (Some(value), false) => s.push_str(&format!(
            "build cache: the main lane shares plain targo's ({value})\n"
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

/// THE RUN'S TOOLCHAIN LEASES (`aterm_toolchain::lease`): the run hands one
/// physical toolchain directory to every child, and between two children no
/// process runs from it, so without a lease atpkg's gc could reclaim it and an
/// unattended trust update could flip it mid-run (two compilers, a rebuild of the
/// graph). Held for the whole run, it keeps the build from gc and makes the
/// unattended flip wait (atpkg's `quiet`, at most its `CEILING_SECS`). A tool the
/// directory lacks and the run took from the managed Trust build
/// ([`Toolchain::borrowed`]) runs out of THAT build, so it is held too: one lease per
/// distinct build the run executes from ([`held_dirs`]). A directory atpkg does not
/// manage takes none; one that cannot be taken is said on stderr and the run goes on
/// as it would have without it. Returns the leases (drop them when the run ends) and
/// the log's sentence about them.
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

/// The directories a run executes tools from: the toolchain directory, then the
/// directory of each tool it took from the managed build ([`Toolchain::borrowed`]),
/// each once, in that order.
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

/// How many of the run logs are kept.
const LOGS_KEPT: usize = 20;

/// Open this run's log: `--log <path>` (read from `cwd`), else
/// `<target>/verify/logs/<epoch>-<pid>.log`, keeping the newest
/// [`LOGS_KEPT`]. A log that cannot be opened costs the record, never the run.
fn open_log(chosen: Option<&Path>, cwd: &Path, ctx: &Ctx) -> Option<(PathBuf, std::fs::File)> {
    let path = match chosen {
        Some(p) => cwd.join(p),
        None => {
            let dir = ctx.state_dir().join("logs");
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

/// Keep the newest [`LOGS_KEPT`] logs in `dir`; best effort.
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

/// The checkout `cwd` is in: `git rev-parse --show-toplevel`.
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

/// The whole CLI, in-process: parse `args` (what follows `verify`), find the
/// checkout `cwd` is in, run, print, and answer the exit code.
#[must_use]
pub fn run_cli(args: &[String], cwd: &Path) -> i32 {
    // `perf`, `soak`, `glue` and the browser lane's verbs (`fetch-browser`,
    // `browser-site`, `browser-drive`) parse their own arguments, and none
    // needs targo: a host without Trust drives the browser lane.
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
    if args.first().is_some_and(|a| a == "fetch-browser") {
        return browser::fetch_main(&args[1..]);
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
    // Held until this function returns: the whole run.
    let (_leases, lease_said) = hold_toolchain(&env, &tools, &root);
    #[cfg(unix)]
    exec::group::kill_on_interrupt();

    // The selection: the cone unless `-p` or `--all` replace it; read only
    // when a selected check reads one.
    let path_env = tools.path_with_stage2_first(&env.path);
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
    // A selection holding aterm-gpu runs its oracle arm with its tests, and one
    // holding a web module its replay.
    let wanted = checks::with_pulled(&parsed.checks, parsed.release, |p| scope.includes_crate(p));
    let ctx = Ctx::new_with_tools(root, scope, env, scratch.clone(), tools)
        .with_checks(&wanted)
        .with_release(parsed.release)
        .with_fix(parsed.fix)
        .with_cells(parsed.cells.clone())
        .with_no_windows(parsed.no_windows)
        .with_measure_artifact(measure_artifact)
        .with_child_ceiling(parsed.timeout.unwrap_or(Some(exec::DEFAULT_CHILD_CEILING)))
        .with_test_jobs(parsed.test_jobs)
        .with_test_threads(parsed.test_threads)
        .with_timing_lock(Some(default_timing_lock()))
        .with_toolchain_lease(lease_said);
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
    std::fs::remove_dir_all(&scratch).ok();
    code
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
/// whatever is there. A test helper, public so the integration suites and the
/// unit tests share one copy.
///
/// macOS assesses each new executable inode on its first exec, one at a time
/// machine-wide, so each distinct text is written once per process and hard-linked
/// into place. The shared file is read-only, so writing over a stand-in in place
/// fails loudly; replace one through here or by `mv`. A text that holds its own path
/// is shared by nothing (`$0` is the link that was run).
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
    // Another filesystem: a plain copy is slower to first exec and as correct.
    std::fs::write(path, &text).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// [`standin`] for a body that varies: installs the constant text `. "$0.body"` at
/// `path` (one inode, one first-exec assessment per process) and writes `body` beside
/// it as `<path>.body`. Sourced, `$0` is still the link and `exit`/`exec` act on the
/// stand-in; a `return` ends it. The body is replaced by `rename`, so a reader sees the
/// old text or the new, never half.
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
    std::env::split_paths(path).any(|d| is_executable_file(&d.join(name)))
}

/// aterm's directories in this user's cache ([`aterm_uds::host::user_cache`]: the one
/// computation the tool resolver reads too), on this host ([`host_user_cache`]). The
/// browser lane unpacks its pinned browsers into `browser`, and the tools a check runs
/// live in `tooling` ([`wasm_glue::tooling`], [`aterm_uds::host::tooling_dir`]).
pub use aterm_uds::host::{host_user_cache, user_cache};

#[cfg(test)]
mod tests {
    use super::*;

    /// A tool the run took from the managed build runs out of THAT build, so the run
    /// holds it for as long as it runs, once however many tools came from it.
    /// NEGATIVE CONTROL: the same toolchain with nothing borrowed holds nothing (its
    /// directory is none of atpkg's), and once the run's leases drop nobody holds it.
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
            // Each stand-in is run once as it is laid, through spawnfd's `ETXTBSY`
            // retry: a sibling test thread forking while this one held the file
            // open for writing hands its child that descriptor, and Linux refuses
            // the exec until the child's own exec closes it. Once one exec has
            // started, discovery's own (a bare std spawn) cannot meet it.
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
            // No current link exists: discovery must use the PATH shim's reported
            // physical sysroot before the run asks the actual lease holder.
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

    /// Every child builds in the caller's `CARGO_TARGET_DIR`, and
    /// `--test-threads` reaches every child only when asked.
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
        // What `cargo run` told this process about itself never reaches a child
        // build: ring's build script tracks CARGO_MANIFEST_DIR.
        for k in ["CARGO_MANIFEST_DIR", "CARGO_PKG_*", "CARGO_PRIMARY_PACKAGE"] {
            assert!(env.remove_env.contains(&k), "{k}");
        }
        let threaded = theirs.with_test_threads(std::num::NonZeroU32::new(3));
        assert!(has(&threaded, "RUST_TEST_THREADS", "3"));
    }

    /// THE MAIN LANE SHARES PLAIN TARGO'S CACHE exactly where atpkg's shim would
    /// hand plain `targo` its build-cache `--config` (macOS, a root under a
    /// writable `$HOME`, no layout chosen): then no child is pinned to a
    /// CARGO_TARGET_DIR, and the target dir is still `<root>/target`. A layout
    /// variable set, even empty, turns it off, as it turns the shim off.
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
                    "build.build-dir='{}/Library/Caches/aterm/cargo-build.noindex\
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
        } else {
            assert_eq!(got, None, "the shim's cache is macOS only");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// WHERE THE SHIM STEPS ASIDE the main lane is pinned to the run's target dir
    /// and its intermediates still go where Spotlight never looks:
    /// `<target>/build.noindex`, so only the uplifted artifacts land in the plain
    /// folder. A caller's own build directory (the variable, even empty, or a
    /// cargo config naming one) is left alone; the pin stays either way.
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
        // No HOME (no shim cache): the checkout's own target dir.
        let c = ctx(Path::new("/repo"), EnvSnapshot::default());
        assert_eq!(pinned(&c), Some(PathBuf::from("/repo/target")));
        assert_eq!(
            c.main_build_dir.as_deref(),
            Some("build.build-dir='/repo/target/build.noindex'")
        );
        // The caller's build directory wins.
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

    /// A caller's own RUST_TEST_THREADS is a pin, as --test-threads is.
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

    /// A rerun keeps `--no-windows` for a check it changes: a windowed stage's
    /// (appkit) and one that skips its windowed rows (browser, whose WebKit
    /// session opens windows). NEGATIVE CONTROL: a check it does not change
    /// (fmt) drops it, and without the flag no rerun gains it.
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
        let git = std::process::Command::new("git")
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

    /// The header says what the selection pulled into `dev`, and the package
    /// that pulled each.
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
    }
}
