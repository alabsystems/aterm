// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-verify` — every aterm check as one CLI: `targo --unverified verify [CHECK|SET ...]`.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the verify CLI spawns its checks"
)]

// Dependencies these fixtures drive flush durably (aterm-capacity's records among them). The
// durable crate judges the hardware flush itself; keep every requested flush here without
// draining the SSD for each fixture.
#[cfg(all(test, unix))]
aterm_durable::plain_durable_flushes_in_tests!();

/// Removing the fixture policy must fail, and keeping it must preserve every requested flush
/// and every injected I/O error. Hardware durability itself belongs to aterm-durable's tests.
#[cfg(all(test, target_vendor = "apple"))]
#[test]
fn fixture_flushes_keep_their_order_and_errors_without_hardware_drains() {
    use aterm_durable::{Flush, seam};
    assert!(aterm_durable::durable_flushes_are_plain());
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = std::fs::File::create(dir.path().join("record")).unwrap();
    let recorded = seam::Recording::start();
    Flush::Durable.file(&file).unwrap();
    Flush::Ordered.file(&file).unwrap();
    let _failure = seam::FailingDirs::with_kind(std::io::ErrorKind::Other);
    assert!(Flush::Durable.dir(dir.path()).is_err());
    assert_eq!(
        recorded.take(),
        [Flush::Durable, Flush::Ordered, Flush::Durable]
    );
}

/// The exact first-exec line B1's frozen release-tool proof expects.
pub const FROZEN_TOOL_VERSION: &str = "aterm-verify tool 1";

#[cfg(all(test, unix, debug_assertions))]
mod benchmark_fixture_tests;

pub mod appkit_content;
pub mod browser;
pub mod cells;
mod census_fix;
pub mod changed;
pub mod changelog_guard;
pub mod checks;
pub mod cli;
pub mod compile_account;
pub mod compile_width;
mod compiler_message;
mod compiler_observation;
/// The disk a run will write, claimed before it builds (`docs/DESIGN-adaptive-build-2026-10-24.md`, DISK-ADMIT).
#[cfg(unix)]
pub mod disk;
pub mod docdriver;
pub mod docscan;
pub mod engine;
pub mod event_log;
pub mod exec;
pub mod export_scan;
pub mod findings;
pub mod fmt;
pub mod git_paths;
pub mod git_provider;
mod harness_debt;
pub mod input_content;
pub mod ladder;
pub mod libtest;
pub mod libtest_json;
pub mod lint_content;
pub mod memory;
pub mod native_verifier;
pub mod own_tests;
pub mod perf;
pub mod plan;
#[cfg(unix)]
pub mod provision;
pub mod release_input;
mod released_shim;
pub mod report;
mod resource_waits;
mod run_log;
/// Nothing outlives its run: the run record, the stage-end survivor check and the next run's start ending a dead run's proven members (`docs/DESIGN-leftover-processes-2026-10-16.md` §3.3, §5.4, §8.1).
pub mod runs;
pub mod rustup;
pub mod sched;
pub mod scope;
pub mod shape;
#[cfg(unix)]
pub mod smoke;
#[cfg(unix)]
pub mod smoke_stages;
pub mod soak;
pub mod stages;
pub mod status;
pub mod test_content;
mod test_env;
pub mod test_history;
pub mod test_index;
pub mod test_plan;
mod test_progress;
mod test_shards;
mod test_workers;
pub mod testrun;
pub mod unitgraph;
/// THE toolchain resolution, shared with atpkg (`aterm help rust`), the libc oracle's gate and the release cutter.
pub use aterm_toolchain::discovery as toolchain;
#[cfg(unix)]
pub mod judged;
/// `verify land`: merge main, judge the cone since it, push, compose or re-judge (design B10).
#[cfg(unix)]
pub mod land;
#[cfg(unix)]
mod remote;
pub mod remote_cli;
pub mod remote_host;
pub mod wasm_glue;
mod wasm_page;
pub mod wasm_smoke;
#[cfg(unix)]
pub mod watch;
pub mod window_list;

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::memory::StageKey as _;
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
    /// Installed check tools, independent of disposable test state.
    pub tool_home: Option<PathBuf>,
    /// Signed package store selected before a test HOME replaces the caller's.
    pub test_package_prefix: Option<PathBuf>,
    pub trust_stage2_bin: Option<PathBuf>,
    /// `XDG_CONFIG_HOME`.
    pub xdg_config_home: Option<PathBuf>,
    /// Linux's login runtime directory, checked before it enters a test child.
    pub xdg_runtime_dir: Option<PathBuf>,
    /// `RUSTUP_HOME`, raw ([`toolchain::rustup_home`]).
    pub rustup_home: Option<OsString>,
    /// `SSH_CONNECTION`.
    pub ssh_connection: Option<String>,
    /// `CARGO_BUILD_JOBS`, raw.
    pub cargo_build_jobs: Option<OsString>,
    /// `BUILD_SLOT_HELD`, raw: a managed run must name its admitted CPU share.
    pub build_slot_held: Option<OsString>,
    /// `ATERM_RELEASE_POOL` ([`aterm_capacity::POOL_ENV`]), raw: the pool a release handed this
    /// run, which [`main`] adopts ([`Ctx::pool`]) and the test run's charges are drawn from
    /// ([`test_workers::share`]). Removed from every child (`ATERM_RELEASE_*` in
    /// [`CHILD_ENV_REMOVED`]): the run charges its children itself.
    pub release_pool: Option<OsString>,
    /// `RUSTDOC` or `CARGO_BUILD_RUSTDOC`.
    pub rustdoc_override: Option<OsString>,
    /// `RUSTC` and `CARGO_BUILD_RUSTC`, each when set: a compiler every compile child would take
    /// in place of the one beside `targo` ([`lock_compiler_refusal`]).
    pub rustc_vars: Vec<(&'static str, String)>,
    /// `CARGO_TARGET_DIR`, absolute.
    pub cargo_target_dir: Option<PathBuf>,
    /// Cargo's explicitly set layout selectors ([`CARGO_LAYOUT_VARS`]). Startup capture anchors
    /// relative environment paths at its working directory; manually supplied values retain
    /// the selected source root as their base.
    pub cargo_layout: Vec<(&'static str, OsString)>,
    /// Explicit C compiler, SDK and native build inputs, captured before test isolation.
    pub native_toolchain: Vec<(OsString, OsString)>,
    /// `RUST_TEST_THREADS`, raw.
    pub rust_test_threads: Option<OsString>,
    /// `CARGO_HOME`.
    pub cargo_home: Option<PathBuf>,
    /// The qualify pilot's opt-in failing-test marker, explicitly declared for test children.
    pub qualify_red_marker: Option<PathBuf>,
    /// The enabled native package prefix, resolved by the package manager.
    pub native_packages: Option<PathBuf>,
    /// `PUBLICATION_ENGINE`, raw: an engine checkout whose HEAD commit the engine's readers
    /// judge (a release hands each child its own); unset, they read the engine origin's main
    /// ([`engine::named`]).
    pub publication_engine: Option<OsString>,
    /// `ATERM_DISK_COVERED` ([`aterm_capacity::DISK_COVER_ENV`]): the name of the disk claim the
    /// work this run is part of holds (the release's), which covers the run's disk
    /// ([`disk::admit`]). Read here alone and removed from every child ([`CHILD_ENV_REMOVED`]),
    /// so a test binary never sees it.
    pub disk_cover: Option<String>,
}

impl EnvSnapshot {
    fn cargo_var(&self, name: &str) -> Option<OsString> {
        if name == "CARGO_TARGET_DIR" && self.cargo_target_dir.is_some() {
            return self
                .cargo_target_dir
                .as_ref()
                .map(|path| path.as_os_str().to_owned());
        }
        if let Some((_, value)) = self.cargo_layout.iter().find(|(key, _)| *key == name) {
            Some(value.clone())
        } else if name == "CARGO_HOME" {
            self.cargo_home
                .as_ref()
                .map(|path| path.as_os_str().to_owned())
        } else {
            None
        }
    }

    /// The caller's captured Cargo selectors for a new verifier, including explicit removals.
    /// Compiler-selected final and intermediate paths never become runtime caller choices.
    /// Captured relative environment paths retain their startup base, while Cargo expands valid
    /// build-directory templates. An absent Cargo home remains absent.
    pub fn cargo_selector_env(
        &self,
    ) -> impl Iterator<Item = (&'static str, Option<OsString>)> + '_ {
        CARGO_LAYOUT_VARS
            .into_iter()
            .map(|name| (name, self.cargo_var(name)))
    }

    /// Restore the original Cargo selectors on a verifier runtime command. Missing selectors
    /// are removed, so a bootstrap compiler's generated placement cannot leak into the run.
    pub fn apply_cargo_selectors(&self, command: &mut aterm_uds::spawnfd::Command) {
        for (name, value) in self.cargo_selector_env() {
            if let Some(value) = value {
                command.env(name, value);
            } else {
                command.env_remove(name);
            }
        }
    }

    fn cargo_layout_at(
        cwd: &Path,
        mut selectors: Vec<(&'static str, OsString)>,
    ) -> Vec<(&'static str, OsString)> {
        for (name, value) in &mut selectors {
            if matches!(
                *name,
                "CARGO_TARGET_DIR" | "CARGO_BUILD_TARGET_DIR" | "CARGO_HOME"
            ) && !value.is_empty()
                && !Path::new(value.as_os_str()).is_absolute()
            {
                *value = cwd.join(value.as_os_str()).into_os_string();
            }
        }
        if selectors
            .iter()
            .any(|(name, _)| *name == "CARGO_BUILD_BUILD_DIR")
        {
            // Only an environment choice is projected. The shared resolver supplies its
            // template/base rules without looking up or injecting Cargo configuration.
            let selected = {
                let lookup = |name: &str| {
                    selectors
                        .iter()
                        .find(|(key, _)| *key == name)
                        .map(|(_, value)| value.clone())
                };
                let machine = aterm_toolchain::build_cache::Machine {
                    home: None,
                    root: None,
                    cwd: Some(cwd),
                    env: &lookup,
                    read: &|_| Ok(None),
                    writable: &|_| Some(false),
                };
                aterm_toolchain::build_cache::cargo_build_dir_hint_with(&machine)
                    .ok()
                    .flatten()
                    .map(|hint| hint.value().to_owned())
            };
            if let Some(selected) = selected
                && let Some((_, value)) = selectors
                    .iter_mut()
                    .find(|(name, _)| *name == "CARGO_BUILD_BUILD_DIR")
            {
                *value = selected;
            }
            // Invalid or empty choices stay unchanged for NativeBuildPlan's ordinary refusal.
        }
        selectors
    }

    /// The variables that name the publication engine ([`engine::named`]), as this snapshot
    /// holds them: `PUBLICATION_ENGINE` and `HOME`.
    #[must_use]
    pub fn engine_var(&self, name: &str) -> Option<OsString> {
        match name {
            "PUBLICATION_ENGINE" => self.publication_engine.clone(),
            "HOME" => Some(self.home.clone().into_os_string()).filter(|h| !h.is_empty()),
            _ => None,
        }
    }

    /// Read the process environment.
    #[must_use]
    pub fn capture() -> Self {
        let cwd = std::env::current_dir().ok();
        let selectors = CARGO_LAYOUT_VARS
            .iter()
            .filter_map(|name| Some((*name, std::env::var_os(name)?)))
            .collect();
        let cargo_layout = match cwd.as_deref() {
            Some(cwd) => Self::cargo_layout_at(cwd, selectors),
            None => selectors,
        };
        let cargo_path = |name: &str| {
            cargo_layout
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| PathBuf::from(value))
        };
        let cargo_target_dir =
            cargo_path("CARGO_TARGET_DIR").filter(|path| !path.as_os_str().is_empty());
        let cargo_home = cargo_path("CARGO_HOME").filter(|path| path.is_absolute());
        let var_path = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let native = atpkg::abrew::prefix::Prefix::resolve();
        let native_packages =
            (native.root.is_absolute() && !native.is_disabled()).then_some(native.root);
        Self {
            path: std::env::var_os("PATH").unwrap_or_default(),
            home: var_path("HOME").unwrap_or_default(),
            tool_home: aterm_uds::host::tool_home(),
            test_package_prefix: var_path("ATERM_TEST_PACKAGE_PREFIX")
                .filter(|p| p.is_absolute())
                .or_else(|| atpkg::store::resolve_configured().map(|layout| layout.prefix)),
            trust_stage2_bin: var_path("TRUST_STAGE2_BIN"),
            xdg_config_home: var_path("XDG_CONFIG_HOME").filter(|p| !p.as_os_str().is_empty()),
            xdg_runtime_dir: var_path("XDG_RUNTIME_DIR"),
            rustup_home: std::env::var_os("RUSTUP_HOME"),
            ssh_connection: std::env::var("SSH_CONNECTION").ok(),
            cargo_build_jobs: std::env::var_os("CARGO_BUILD_JOBS"),
            build_slot_held: std::env::var_os("BUILD_SLOT_HELD"),
            release_pool: std::env::var_os(aterm_capacity::POOL_ENV),
            rustdoc_override: std::env::var_os("RUSTDOC")
                .or_else(|| std::env::var_os("CARGO_BUILD_RUSTDOC")),
            rustc_vars: aterm_toolchain::rustc_movers::RUSTC_VARS
                .iter()
                .filter_map(|k| Some((*k, std::env::var_os(k)?.to_string_lossy().into_owned())))
                .collect(),
            cargo_target_dir,
            cargo_home,
            qualify_red_marker: var_path("ATERM_QUALIFY_RED_MARKER").filter(|p| p.is_absolute()),
            cargo_layout,
            native_toolchain: test_env::native_inputs(std::env::vars_os()),
            rust_test_threads: std::env::var_os("RUST_TEST_THREADS"),
            native_packages,
            publication_engine: std::env::var_os("PUBLICATION_ENGINE").filter(|v| !v.is_empty()),
            disk_cover: std::env::var(aterm_capacity::DISK_COVER_ENV)
                .ok()
                .filter(|m| !m.trim().is_empty()),
        }
    }
}

#[cfg(all(test, unix))]
mod runtime_selector_tests;

/// Keep the caller's choices first, then expose enabled native package tools
/// to build and test children even when the invoking agent predates shellenv.
fn native_package_path(path: &OsStr, prefix: Option<&Path>) -> OsString {
    let Some(prefix) = prefix.filter(|prefix| prefix.is_absolute()) else {
        return path.to_os_string();
    };
    let mut paths: Vec<PathBuf> = std::env::split_paths(path).collect();
    for dir in [prefix.join("bin"), prefix.join("sbin")] {
        if !paths.contains(&dir) {
            paths.push(dir);
        }
    }
    std::env::join_paths(paths).unwrap_or_else(|_| path.to_os_string())
}

/// The variables that move cargo's target or build directory, and the home whose config may.
pub const CARGO_LAYOUT_VARS: [&str; 4] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_BUILD_DIR",
    "CARGO_HOME",
];

/// The `build.build-dir` value this version's atpkg shim chooses for `root`
/// (`aterm_toolchain::build_cache::decide`), using the quiet cache regardless of an older
/// installed shim's path.
#[must_use]
pub fn main_build_dir(env: &EnvSnapshot, root: &Path) -> Option<String> {
    use aterm_toolchain::build_cache::{BuildDir, Machine, Tool, cache_root, decide, read_config};
    if !cfg!(target_os = "macos") {
        return None;
    }
    let lookup = |key: &str| env.cargo_var(key);
    let cache = cache_root(&env.home);
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

/// `path` spelled physically: its longest existing ancestor through every symlink
/// (`aterm_uds::runs::canonical`), the rest appended. Off unix it is `path`, since there
/// `canonicalize` spells a verbatim path no tool expects.
#[must_use]
pub fn physical(path: &Path) -> PathBuf {
    if cfg!(unix) {
        aterm_uds::runs::canonical(path)
    } else {
        path.to_path_buf()
    }
}

/// A native compiler's immutable target and intermediate selection. Resolution creates nothing;
/// automatic final directories remain lexical until descriptor preparation checks every component.
#[derive(Clone, Debug)]
pub struct NativeBuildPlan {
    env: EnvSnapshot,
    root: PathBuf,
    target: PathBuf,
    default_root: Option<PathBuf>,
    shared_build_dir: Option<String>,
    build_dir: Option<String>,
    compiler_layout: Result<CompilerLayout, String>,
}

#[derive(Clone, Debug)]
struct CompilerLayout {
    cargo_home: Option<PathBuf>,
    caller_build_dir: Option<aterm_toolchain::build_cache::CargoBuildDirHint>,
}

impl NativeBuildPlan {
    /// Capture Cargo's target choice from this environment and the selected workspace's config.
    /// Caller-selected final paths are physical; the automatic path stays under the physical root.
    ///
    /// # Errors
    /// Cargo's target selection is empty, unreadable, malformed or has unresolved configuration.
    pub fn capture(env: &EnvSnapshot, root: &Path) -> std::io::Result<Self> {
        use aterm_toolchain::build_cache::{Machine, cargo_target_hint_with, read_config};
        let root = physical(root);
        let lookup = |key: &str| env.cargo_var(key);
        let machine = Machine {
            home: Some(env.home.as_path()).filter(|home| !home.as_os_str().is_empty()),
            root: None,
            cwd: Some(&root),
            env: &lookup,
            read: &read_config,
            writable: &writable_dir,
        };
        let selected = cargo_target_hint_with(&machine)
            .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidInput, why))?;
        let default_root = selected.is_none().then(|| root.clone());
        let target = selected.map_or_else(
            || root.join(aterm_toolchain::build_cache::DEFAULT_TARGET),
            |target| physical(&target),
        );
        let plan = Self::selected(env, root, target, default_root);
        plan.layout()?;
        Ok(plan)
    }

    /// Capture an actual explicit native `--target-dir`, independently of Cargo's configured target.
    #[must_use]
    pub fn for_target(env: &EnvSnapshot, root: &Path, target: &Path) -> Self {
        let root = physical(root);
        let target = if target.is_absolute() {
            target.to_path_buf()
        } else {
            root.join(target)
        };
        Self::selected(env, root, physical(&target), None)
    }

    /// Capture a generated final target under an artifact root independent of the source tree.
    /// Source configuration still selects intermediates; cleaning the source cannot remove finals.
    #[must_use]
    pub fn for_default_target(env: &EnvSnapshot, source_root: &Path, target_root: &Path) -> Self {
        let target_root = physical(target_root);
        let target = target_root.join(aterm_toolchain::build_cache::DEFAULT_TARGET);
        Self::selected(env, physical(source_root), target, Some(target_root))
    }

    fn selected(
        env: &EnvSnapshot,
        root: PathBuf,
        target: PathBuf,
        default_root: Option<PathBuf>,
    ) -> Self {
        use aterm_toolchain::build_cache::{
            Machine, cargo_build_dir_hint_with, cargo_home_hint_with, read_config,
        };
        let lookup = |name: &str| env.cargo_var(name);
        let machine = Machine {
            home: Some(env.home.as_path()).filter(|home| !home.as_os_str().is_empty()),
            root: None,
            cwd: Some(&root),
            env: &lookup,
            read: &read_config,
            writable: &writable_dir,
        };
        let compiler_layout = cargo_home_hint_with(&machine).and_then(|cargo_home| {
            Ok(CompilerLayout {
                cargo_home,
                caller_build_dir: cargo_build_dir_hint_with(&machine)?,
            })
        });
        let mut env = env.clone();
        if let Ok(layout) = &compiler_layout {
            env.cargo_layout.retain(|(name, _)| *name != "CARGO_HOME");
            if let Some(home) = &layout.cargo_home {
                env.cargo_layout
                    .push(("CARGO_HOME", home.as_os_str().to_owned()));
            }
        }
        let generated = compiler_layout
            .as_ref()
            .is_ok_and(|layout| layout.caller_build_dir.is_none());
        let shared_build_dir = if generated {
            main_build_dir(&env, &root)
        } else {
            None
        };
        let build_dir = if generated {
            shared_build_dir
                .clone()
                .or_else(|| fallback_build_dir(&env, &root, &target))
        } else {
            None
        };
        Self {
            env,
            root,
            target,
            default_root,
            shared_build_dir,
            build_dir,
            compiler_layout,
        }
    }

    /// The exact final directory every child receives as `--target-dir` or `CARGO_TARGET_DIR`.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// The exact intermediate-directory configuration selected with this target.
    #[must_use]
    pub fn build_dir(&self) -> Option<&str> {
        self.build_dir.as_deref()
    }

    fn layout(&self) -> std::io::Result<&CompilerLayout> {
        self.compiler_layout
            .as_ref()
            .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidInput, why.clone()))
    }

    /// Whether the caller selected intermediates, including valid opaque Cargo templates.
    #[must_use]
    pub fn has_explicit_build_dir(&self) -> bool {
        self.compiler_layout
            .as_ref()
            .is_ok_and(|layout| layout.caller_build_dir.is_some())
    }

    /// The absolute lexical caller path, when Cargo need not compute a template to name it.
    #[must_use]
    pub fn explicit_build_dir(&self) -> Option<&Path> {
        self.compiler_layout
            .as_ref()
            .ok()?
            .caller_build_dir
            .as_ref()?
            .path()
    }

    /// Set/remove projection for every Cargo layout variable. Values come only from this plan;
    /// valid Cargo templates remain unexpanded for the command's actual manifest workspace.
    ///
    /// # Errors
    /// The captured layout was invalid or unresolved.
    pub fn compiler_env(
        &self,
    ) -> std::io::Result<impl Iterator<Item = (&'static str, Option<&OsStr>)>> {
        Ok(self.layout_env(self.layout()?))
    }

    fn layout_env<'a>(
        &'a self,
        layout: &'a CompilerLayout,
    ) -> impl Iterator<Item = (&'static str, Option<&'a OsStr>)> {
        [
            ("CARGO_TARGET_DIR", Some(self.target.as_os_str())),
            ("CARGO_BUILD_TARGET_DIR", None),
            (
                "CARGO_BUILD_BUILD_DIR",
                layout
                    .caller_build_dir
                    .as_ref()
                    .map(|choice| choice.value()),
            ),
            (
                "CARGO_HOME",
                layout.cargo_home.as_deref().map(Path::as_os_str),
            ),
        ]
        .into_iter()
    }

    /// Apply the captured set/remove projection as command-local compiler environment.
    ///
    /// # Errors
    /// The captured layout was invalid or unresolved.
    pub fn apply_to_command(
        &self,
        command: &mut aterm_uds::spawnfd::Command,
    ) -> std::io::Result<()> {
        for (name, value) in self.compiler_env()? {
            if let Some(value) = value {
                command.env(name, value);
            } else {
                command.env_remove(name);
            }
        }
        Ok(())
    }

    fn apply_to_cmd(&self, layout: &CompilerLayout, mut command: exec::Cmd) -> exec::Cmd {
        for (name, value) in self.layout_env(layout) {
            command = if let Some(value) = value {
                command.env(name, value)
            } else {
                command.remove_env(name)
            };
        }
        command
    }

    fn prepare_target(&self) -> std::io::Result<()> {
        self.layout()?;
        if let Some(root) = &self.default_root {
            atpkg::build_cache::registered::prepare_default_target(root)?;
        }
        Ok(())
    }

    /// Prepare the captured automatic final path and selected intermediates without reselecting
    /// a target from later configuration or environment. Final target selection alone grants no custody.
    ///
    /// # Errors
    /// The automatic final path or selected fallback cannot be prepared safely.
    pub fn prepare(&self) -> std::io::Result<()> {
        self.prepare_target()?;
        prepare_selected_build_dir(
            &self.env,
            &self.root,
            &self.target,
            self.build_dir(),
            self.default_root.is_some(),
        )
    }
}

/// The `build.build-dir` for main-lane children when [`main_build_dir`] is `None` and `target` is pinned.
#[must_use]
pub fn fallback_build_dir(env: &EnvSnapshot, root: &Path, target: &Path) -> Option<String> {
    use aterm_toolchain::build_cache::{Machine, read_config, target_build_dir};
    let lookup = |key: &str| env.cargo_var(key);
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

/// Native children bypass atpkg's shell shim. Prepare their captured automatic final target
/// independently of intermediate placement, binding every child path to that same plan.
fn prepare_main_build_dir(ctx: &Ctx) -> std::io::Result<()> {
    let plan = ctx
        .native_build_plan()
        .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidInput, why.clone()))?;
    let child_target = ctx
        .child_env_add
        .iter()
        .rev()
        .find(|(name, _)| name == "CARGO_TARGET_DIR")
        .map(|(_, value)| value.as_os_str());
    if plan.root != ctx.root
        || plan.target != ctx.target_dir
        || plan.build_dir != ctx.main_build_dir
        || child_target != Some(plan.target.as_os_str())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the captured native build plan differs from its child configuration",
        ));
    }
    if ctx.checks.iter().copied().any(disk::builds) {
        plan.prepare()
    } else {
        plan.prepare_target()
    }
}

/// Prepare an exact producer-selected fallback before a native compiler starts. Layout
/// resolution stays pure: a caller's override or a shared cache receives no new authority.
///
/// # Errors
/// The generated fallback is no longer eligible, or its private directory or managed custody
/// cannot be established.
fn prepare_selected_build_dir(
    env: &EnvSnapshot,
    root: &Path,
    target: &Path,
    selected: Option<&str>,
    automatic_target: bool,
) -> std::io::Result<()> {
    use aterm_toolchain::build_cache::{Machine, Tool, read_config};
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let generated = aterm_toolchain::build_cache::target_build_config(target);
    if selected.is_none() || selected != generated.as_deref() {
        return Ok(());
    }
    if fallback_build_dir(env, root, target).as_deref() != selected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the selected build-directory fallback changed before native admission; no compiler started",
        ));
    }
    let lookup = |key: &str| {
        if key == "CARGO_TARGET_DIR" {
            Some(target.as_os_str().to_owned())
        } else {
            env.cargo_var(key)
        }
    };
    let machine = Machine {
        home: Some(env.home.as_path()).filter(|home| !home.as_os_str().is_empty()),
        root: None,
        cwd: Some(root),
        env: &lookup,
        read: &read_config,
        writable: &writable_dir,
    };
    if !automatic_target {
        atpkg::build_cache::registered::observe_command_with(
            &["--unverified".into(), "check".into()],
            Tool::Targo,
            &machine,
        )?;
    }
    // Scope limits custody, never the safety of a directory this producer selected. An
    // external workspace receives no receipt but cannot redirect its new intermediates.
    atpkg::build_cache::registered::prepare_target_build_dir(target)
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

/// Maximum byte size for a single regular file read by `aterm-verify` (128 MiB).
pub(crate) const MAX_VERIFY_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// Open a regular file without blocking on FIFOs.
pub(crate) fn open_regular(path: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    let path = path.as_ref();
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// Read a regular file up to [`MAX_VERIFY_FILE_BYTES`] without blocking on FIFOs.
pub(crate) fn read_regular(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    aterm_uds::atomic_file::read_regular(path.as_ref(), MAX_VERIFY_FILE_BYTES)
        .map(|(bytes, _)| bytes)
}

/// Read a regular UTF-8 file up to [`MAX_VERIFY_FILE_BYTES`] without blocking on FIFOs.
pub(crate) fn read_regular_text(path: impl AsRef<Path>) -> std::io::Result<String> {
    aterm_uds::atomic_file::read_regular_text(path.as_ref(), MAX_VERIFY_FILE_BYTES)
}

/// Read what a child of verify wrote into a file verify made for it (its captured stdout or
/// stderr, a check's log): a regular file opened without blocking on FIFOs, whatever its size.
/// Its size is the child's, as a pipe's would be, never [`MAX_VERIFY_FILE_BYTES`]: the
/// workspace's `--no-dedupe` test forest for aarch64-unknown-linux-gnu is over 500 MB.
pub(crate) fn read_captured(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    aterm_uds::atomic_file::read_regular(path.as_ref(), u64::MAX).map(|(bytes, _)| bytes)
}

/// Write `bytes` to a fresh regular file at `path` (unlinking any existing entry first and
/// creating exclusively with `create_new(true)`).
pub(crate) fn write_fresh(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    let path = path.as_ref();
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    opts.open(path)?.write_all(bytes.as_ref())
}

/// Copy a regular file at `from` (`O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`) to a fresh regular file
/// at `to` (`create_new(true)` + `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`), preserving permissions.
pub(crate) fn copy_regular_fresh(
    from: impl AsRef<Path>,
    to: impl AsRef<Path>,
) -> std::io::Result<u64> {
    let (mut reader, meta) = aterm_uds::regular_file::open(from.as_ref())?;
    let to = to.as_ref();
    let _ = std::fs::remove_file(to);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let mut writer = opts.open(to)?;
    let copied = std::io::copy(&mut reader, &mut writer)?;
    writer.set_permissions(meta.permissions())?;
    Ok(copied)
}

/// Copy a regular file at `from` (`O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`) to a fresh regular file
/// at `to` (`create_new(true)` + `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`), setting `mode` on the
/// open destination descriptor.
#[cfg(unix)]
pub(crate) fn copy_regular_fresh_mode(
    from: impl AsRef<Path>,
    to: impl AsRef<Path>,
    mode: u32,
) -> std::io::Result<u64> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let (mut reader, _) = aterm_uds::regular_file::open(from.as_ref())?;
    let to = to.as_ref();
    let _ = std::fs::remove_file(to);
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(to)?;
    let copied = std::io::copy(&mut reader, &mut writer)?;
    writer.set_permissions(std::fs::Permissions::from_mode(mode))?;
    Ok(copied)
}

/// Open a fresh regular file at `path` suitable for child `Stdio` redirection (`O_NOFOLLOW | O_CLOEXEC`,
/// without `O_NONBLOCK` so the child's stdio description stays blocking).
pub(crate) fn create_stdio_file(path: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
    let path = path.as_ref();
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    opts.open(path)
}

/// Set directory permissions using the shared descriptor/nofollow recovery operation.
#[cfg(unix)]
pub(crate) fn set_dir_mode_nofollow(dir: impl AsRef<Path>, mode: u32) -> std::io::Result<()> {
    aterm_uds::private_file::set_dir_mode_nofollow(dir.as_ref(), mode)
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
        let plist = read_regular_text(app.join("Contents/Info.plist")).unwrap_or_default();
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
            c.len() >= 7 && c.bytes().all(|b| b.is_ascii_hexdigit()) && head.starts_with(c)
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
    /// The checkout's root, physical (every symlink resolved).
    pub root: PathBuf,
    /// The captured Cargo target, else `<root>/target/.ubd/main.noindex` ([`NativeBuildPlan`]):
    /// the `CARGO_TARGET_DIR` of every child but a side lane's ([`Self::child_env_add`]).
    pub target_dir: PathBuf,
    native_build_plan: Result<NativeBuildPlan, String>,
    /// The checks this run runs, in [`checks::CHECKS`] order.
    pub checks: Vec<Check>,
    /// The `release` set.
    pub release: bool,
    /// `--fix`: `fmt` rewrites the files it finds unformatted.
    pub fix: bool,
    /// `--cell`: the forge cells `cells` checks.
    pub cells: Vec<String>,
    /// The selection pulled `cells` ([`checks::pulls_cells`]): one cell per other OS and one of
    /// this OS for its lint pass, each checked only where its graph holds a selected crate
    /// ([`cells::pulled_cells`]).
    pub cells_pulled: bool,
    pub scope: Scope,
    /// Explicit committed paths for a release's unchecked input; ordinary runs use their diff.
    pub input_paths: Option<Vec<String>>,
    pub tools: Toolchain,
    /// PATH for every child.
    pub path_env: OsString,
    /// Private scratch directory for captured child output.
    pub scratch: PathBuf,
    pub env: EnvSnapshot,
    /// Variables removed from every child's environment ([`CHILD_ENV_REMOVED`]).
    pub child_env_remove: Vec<&'static str>,
    /// Variables given to every child, after the removals and before a stage's own
    /// ([`exec::ExecEnv::add_env`]): `CARGO_TARGET_DIR` (the physical [`Self::target_dir`]) and,
    /// with a targo, `TRUST_STAGE2_BIN`.
    pub child_env_add: Vec<(OsString, OsString)>,
    /// The `build.build-dir` the main lane's children are handed as `--config`.
    pub main_build_dir: Option<String>,
    /// Side-lane artifacts under the managed shared build cache, when selected.
    /// Caller-selected layouts keep side lanes beside their target directory.
    pub cached_lanes_dir: Option<PathBuf>,
    /// What the run's toolchain lease is, for the log ([`hold_toolchain`]).
    pub toolchain_lease: Option<String>,
    /// What the run says of the tree's `trust.lock` beyond the build it names ([`lock_notes`]).
    pub lock_notes: Vec<String>,
    /// HEAD, in full, when the run's selection was read ([`report::moved_checkout`]); unset, the
    /// run reads it as it begins.
    pub head_at_start: Option<String>,
    /// The ceiling on one stage child's work, its waits for admission, a lock or a core left out
    /// ([`exec::ExecEnv::child_ceiling`]).
    pub child_ceiling: Option<Duration>,
    /// `--no-windows`.
    pub no_windows: bool,
    /// Fresh console-lock reading for native-window stages. Inert in constructed test contexts.
    pub screen_locked: fn() -> bool,
    /// A focused GUI pacing measurement, never release qualification.
    pub cursor_pacing: bool,
    /// Run only the test compilation stage, never a release qualification.
    pub compile_only: bool,
    /// The test children carry `ATERM_SPEC_CHECKERS=1` and the test
    /// run owes [`checks::GOLDEN`]'s test: under `release`, whose run of the suite that is, and
    /// under `--as-release`, which compiles it or reruns it.
    pub checkers_in_tests: bool,
    /// `--app <APP>`.
    pub measure_artifact: Option<MeasureArtifact>,
    /// Requested `--test-threads`, else the caller's own `RUST_TEST_THREADS`: the widest a test
    /// binary runs ([`test_plan`]).
    pub test_threads: Option<u32>,
    /// Requested maximum test CPU share, still subject to admission and pool limits.
    pub test_cores: Option<u32>,
    /// Explicit libtest shuffle seed; none preserves its ordinary case order.
    pub test_seed: Option<u64>,
    /// The program cargo runs in place of test binaries ([`testrun::RECORD_FLAG`]) and rustdoc ([`docdriver`]).
    pub test_recorder: Option<PathBuf>,
    /// How the timing stages ([`plan::TIMED`]), and the browser lane where Safari's window takes
    /// the desk (`browser::takes_the_desk`), take the quiet machine: the run waits in the
    /// per-user capacity ledger for measured admission before its first (`Some(Wait)`), and
    /// retains bounded protection through its stages. `Some(Now)` cannot qualify Timing: a
    /// measurement stage starts no callback, while the AppKit correctness drivers make one
    /// non-overriding attempt to own the per-user window admission, then run and report their
    /// live load without claiming measurement qualification. `None` is the caller's explicit
    /// untimed/outer-owned contract; it supplies no CPU observation or measurement qualification.
    pub quiet: Option<aterm_capacity::Admit>,
    /// A repeatable watch sample whose stage admissions rank behind overlapping Ordinary work.
    pub background: bool,
    /// This run's record of its stage groups ([`runs::Runs`]), when it keeps one.
    pub runs: Option<runs::Runs>,
    /// Where the run keeps what each stage's memory peaked at, and the machine it claims on
    /// ([`memory::Book`], VERIFY-MEM); `None` measures nothing and keeps nothing.
    pub memory: Option<memory::Book>,
    /// Where the run keeps what each test binary took ([`test_history`]), which its test runs plan
    /// by ([`test_plan`]): planning data, never a verdict. `None` keeps nothing and plans every
    /// binary as unseen, as every run a test makes does.
    pub test_history: Option<PathBuf>,
    /// The complete test proofs a run may carry ([`testrun::carry`], `--passed-binaries`).
    pub passed_binaries: Option<testrun::Passed>,
    /// Per-package lint successes a release resume may carry by complete content key.
    pub passed_lint_packages: Option<lint_content::Passed>,
    /// Prior successful AppKit groups whose current executable bytes must be re-keyed before
    /// their WindowServer work can be carried.
    pub passed_appkit_groups: Option<appkit_content::Passed>,
    /// Source-reader candidates from the immediately preceding preflight attempt.
    pub input_items: Vec<input_content::Proof>,
    /// The release cutter's frozen verifier, seal and toolchain context, salted into each test
    /// binary and lint-package content key (`--carry-context`).
    pub carry_context: Option<String>,
    /// A development test run brackets each test binary's executable and declared runtime
    /// inputs, as a release's does (`--input-drift`, [`testrun::evidence_context`]): a successful
    /// binary whose inputs changed while it ran makes the test check NOT RUN. It carries nothing.
    pub input_drift: bool,
    /// Where a run writes each `check-finished`, `test-binary-finished` and
    /// `lint-package-finished` line whole, apart from its output, and its check-terminal
    /// `run-finished` line (`--events`; later accounting may follow): a release's cutter keeps its
    /// records from it (A19), and `verify land` its judgment (B10).
    pub events: Option<event_log::EventLog>,
    /// Owner-private directory beside the run log where a hung test binary's raw all-thread
    /// sample is retained. It is created lazily only when the detector fires.
    pub hang_samples: Option<PathBuf>,
    /// A call of the run's test compile did not pass: every later call of it, and the test
    /// run's own compile, are COULD NOT RUN behind it and start no cargo, since a recompile of
    /// what failed would run under a claim learned when it passed, or none.
    pub test_compile_failed: std::sync::atomic::AtomicBool,
    /// What the run's first proven red ends ([`halt_of`]): the tree half finishes its sweep
    /// unless explicitly fail-fast; other releases halt, and ordinary runs halt at preflight.
    pub halt: sched::Halt,
    /// What resolves a call's unit graph for the test compile's planner ([`shape::shape`]). The
    /// boolean marks the exact closing call, whose graph includes every command folded into that
    /// stage; [`stages::unit_graph`] resolves them without building, while tests supply fixtures.
    pub unit_graph:
        fn(&Ctx, &shape::Piece, bool, &mut unitgraph::Interner) -> Result<unitgraph::Graph, String>,
    /// What enumerates the workspace members for adaptive test-compile shaping. Production reads
    /// Cargo's resolved workspace; a planner test supplies a fixed graph beside [`Ctx::unit_graph`].
    pub workspace_members: fn(&Ctx) -> changed::Members,
    /// What the run's shaped test compile has built so far, and what the fresh check said
    /// ([`shape::fresh_check`]).
    pub shape_run: Mutex<shape::Run>,
    /// When the run began (its context was made), which a release run's record of what it used
    /// names ([`disk::keep_used`]): a unit of the main lane's build directory written since is one
    /// some compile of the run built.
    pub began: std::time::SystemTime,
    /// What names each compiler of a test compile for its unit, its span and, where it is heavy,
    /// its unit's record ([`memory::read_unit`] but in a test).
    pub unit_reader: memory::UnitReader,
    /// The release's pool this run adopted from [`EnvSnapshot::release_pool`] ([`run_cli`]):
    /// the test run's share, its binaries' charges held in its tokens
    /// ([`testrun::dispatch_pooled`]); `None` without an adopted pool. Shared ownership keeps
    /// bounded workers' tokens alive until their actual completion.
    pub pool: Option<std::sync::Arc<aterm_capacity::Jobserver>>,
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

/// Variables removed from every child's inherited environment: checker and release policy, what `cargo run` says of the
/// verify process ([`CARGO_RUN_ENV`]), and the disk claim that covers this run
/// ([`aterm_capacity::DISK_COVER_ENV`], [`EnvSnapshot::disk_cover`]), which covers no child's
/// own claim and which a test binary must never read (a release's children would then run its
/// tests unlike a developer's). All test commands explicitly inject their own
/// marker and disposable HOME through `test_env`, including in a Background main lane.
pub const CHILD_ENV_REMOVED: [&str; 10] = {
    let mut all = [aterm_capacity::DISK_COVER_ENV; 10];
    all[8] = stages::CHECKERS_ENV;
    all[9] = "ATERM_RELEASE_*";
    let mut i = 0;
    while i < CARGO_RUN_ENV.len() {
        all[i] = CARGO_RUN_ENV[i];
        i += 1;
    }
    all
};

/// The run's toolchain, discovered once from `env` under the pin `root` declares, the atpkg prefix
/// atpkg itself would read and the `trust.lock` at or above `root` (`atpkg::trust_lock::found_for`:
/// a locked tree compiles with the build its lock names, or not at all).
#[must_use]
pub fn run_toolchain(env: &EnvSnapshot, root: &Path) -> Toolchain {
    let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    Toolchain::discover_with_store(
        env.trust_stage2_bin.as_deref(),
        &toolchain::rustup_home(env.rustup_home.as_deref(), &env.home),
        Some(&prefix),
        &env.path,
        toolchain::pinned_channel(root).as_deref(),
        atpkg::trust_lock::found_for(root, Some(&prefix)).as_ref(),
    )
}

impl Ctx {
    pub(crate) fn native_build_plan(&self) -> Result<&NativeBuildPlan, String> {
        let plan = self.native_build_plan.as_ref().map_err(Clone::clone)?;
        plan.layout().map_err(|why| why.to_string())?;
        Ok(plan)
    }

    /// Stage construction is infallible; admission reports a captured-plan refusal before
    /// any stage runs. Valid compiler commands carry their complete layout locally.
    pub(crate) fn compiler_command(&self, command: exec::Cmd) -> exec::Cmd {
        match &self.native_build_plan {
            Ok(plan) => match &plan.compiler_layout {
                Ok(layout) => plan.apply_to_cmd(layout, command),
                Err(_) => command,
            },
            Err(_) => command,
        }
    }
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
        let _ = report::running_driver();
        let path_env = native_package_path(
            &tools.path_with_stage2_first(&env.path),
            env.native_packages.as_deref(),
        );
        let root = physical(&root);
        let native_build_plan =
            NativeBuildPlan::capture(&env, &root).map_err(|why| why.to_string());
        let target_dir = native_build_plan.as_ref().map_or_else(
            |_| root.join(aterm_toolchain::build_cache::DEFAULT_TARGET),
            |plan| plan.target().to_path_buf(),
        );
        // Physical as the target is: a side lane hands its children this as their target.
        let cached_lanes_dir = native_build_plan
            .as_ref()
            .ok()
            .and_then(|plan| plan.shared_build_dir.as_deref())
            .and_then(|value| disk::build_dir_path(value, &root))
            .map(|dir| physical(&dir.join("verify").join(aterm_tempfile::QUIET)));
        // The main lane names its build directory itself (`--config build.build-dir`), so the
        // target this pins never moves the shared intermediates; a side lane sets its own.
        let mut child_env_add: Vec<(OsString, OsString)> = vec![(
            "CARGO_TARGET_DIR".into(),
            target_dir.clone().into_os_string(),
        )];
        let main_build_dir = native_build_plan
            .as_ref()
            .ok()
            .and_then(|plan| plan.build_dir().map(str::to_owned));
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
            native_build_plan,
            checks: checks::DEV.to_vec(),
            release: false,
            fix: false,
            cells: Vec::new(),
            cells_pulled: false,
            scope,
            input_paths: None,
            tools,
            path_env,
            scratch,
            env,
            child_env_remove: CHILD_ENV_REMOVED.to_vec(),
            child_env_add,
            main_build_dir,
            cached_lanes_dir,
            toolchain_lease: None,
            lock_notes: Vec::new(),
            head_at_start: None,
            child_ceiling: Some(exec::DEFAULT_CHILD_CEILING),
            no_windows: false,
            screen_locked: || false,
            cursor_pacing: false,
            compile_only: false,
            checkers_in_tests: false,
            measure_artifact: None,
            test_threads: pinned_threads,
            test_cores: None,
            test_seed: None,
            test_recorder: std::env::current_exe().ok(),
            quiet: None,
            background: false,
            runs: None,
            memory: None,
            test_history: None,
            passed_binaries: None,
            passed_lint_packages: None,
            passed_appkit_groups: None,
            input_items: Vec::new(),
            carry_context: None,
            input_drift: false,
            events: None,
            hang_samples: None,
            test_compile_failed: std::sync::atomic::AtomicBool::new(false),
            halt: sched::Halt::off(),
            unit_graph: stages::unit_graph,
            workspace_members: |ctx| changed::read_members(&ctx.root, &ctx.tools, &ctx.path_env),
            shape_run: Mutex::new(shape::Run::default()),
            began: std::time::SystemTime::now(),
            unit_reader: memory::read_unit,
            pool: None,
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
        self.checkers_in_tests = release;
        self
    }

    /// The halt's words once the run's first proven red ended it ([`Ctx::halt`]); none while it
    /// has not, and none when an interrupt was ending the run before its red, so what that run
    /// stopped keeps the interrupt's words as its children do ([`exec::group::halt`]).
    #[must_use]
    pub fn halted(&self) -> Option<&str> {
        let words = self.halt.words()?;
        (!stopping() || exec::halted().is_some()).then_some(words)
    }

    /// What the run's first proven red ends ([`Ctx::halt`]).
    #[must_use]
    pub fn with_halt(mut self, halt: sched::Halt) -> Self {
        self.halt = halt;
        self
    }

    /// `--as-release` ([`Ctx::checkers_in_tests`]).
    #[must_use]
    pub const fn with_checkers_in_tests(mut self, checkers: bool) -> Self {
        self.checkers_in_tests = checkers;
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

    /// This process's parallelism ([`aterm_cores::available_parallelism`]: the cores it may run
    /// on, which an affinity mask, a cgroup quota or a verifier's grant narrows), at least one:
    /// what bounds the run's own direct concurrency. The machine's cores are [`aterm_capacity::cores`].
    #[must_use]
    pub fn parallelism(&self) -> u32 {
        aterm_cores::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX))
    }

    /// The most compiler jobs a side lane may take.
    #[must_use]
    pub fn build_jobs_ceiling(&self) -> Option<u32> {
        parse_build_jobs(self.env.cargo_build_jobs.as_deref())
    }

    /// The CPU share the test binaries are charged from ([`test_workers::share`]): the release's
    /// pool's slots where a pool was adopted, the admitted share where a slot is held
    /// ([`test_workers::admitted_share`]), else this process's parallelism ([`Ctx::parallelism`]).
    /// A context inspected before [`run_with`] refuses malformed admission data (or a pool named
    /// and not adopted) still names no unbounded share: one core. `--test-cores` caps this
    /// request without granting capacity; the execution stage can receive less from admission.
    #[must_use]
    pub fn test_share(&self) -> u32 {
        let available = match test_workers::share(&self.env, self.pool.as_deref()) {
            Ok(test_workers::Share::Pool(slots)) => slots,
            Ok(test_workers::Share::Claim(share)) => share.unwrap_or_else(|| self.parallelism()),
            Err(_) => 1,
        }
        .max(1);
        self.test_cores.map_or(available, |n| n.clamp(1, available))
    }

    /// The most libtest threads one test binary runs on, within `share` cores: `--test-threads`,
    /// else the caller's `RUST_TEST_THREADS`, else the whole share.
    #[must_use]
    pub fn test_threads_within(&self, share: u32) -> u32 {
        let share = share.max(1);
        self.test_threads.map_or(share, |t| t.clamp(1, share))
    }

    /// The most libtest threads one test binary runs on, within [`Ctx::test_share`]; each
    /// recorded binary runs at the width its plan gives it, at most this ([`test_plan::plan`]).
    #[must_use]
    pub fn test_threads(&self) -> u32 {
        self.test_threads_within(self.test_share())
    }

    /// The `RUST_TEST_THREADS` every test child carries where a slot is held, a release's pool
    /// was adopted or the caller pinned it: what the recording pass, the doctests and a rerun
    /// give each binary.
    pub(crate) fn test_thread_override(&self) -> Option<u32> {
        (self.env.build_slot_held.is_some()
            || self.pool.is_some()
            || self.test_threads.is_some()
            || self.test_cores.is_some())
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

    /// The release's pool this run adopted ([`Ctx::pool`]): the test run's share, so the widest
    /// binary its builders and recording pass are pinned to is drawn from it.
    #[must_use]
    pub fn with_pool(mut self, pool: Option<aterm_capacity::Jobserver>) -> Self {
        self.pool = pool.map(std::sync::Arc::new);
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

    /// Request at most this test CPU share; this grants no capacity itself.
    #[must_use]
    pub fn with_test_cores(mut self, cores: Option<std::num::NonZeroU32>) -> Self {
        self.test_cores = cores.map(std::num::NonZeroU32::get);
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

    /// Inject the console-lock source used before native-window stages.
    #[must_use]
    pub const fn with_screen_lock_source(mut self, source: fn() -> bool) -> Self {
        self.screen_locked = source;
        self
    }

    /// Select only the GUI cursor-pacing burst.
    #[must_use]
    pub const fn with_cursor_pacing(mut self, focused: bool) -> Self {
        self.cursor_pacing = focused;
        self
    }

    /// Compile tests with the ordinary test stage's cache and resource admission.
    pub const fn with_compile_only(mut self, compile_only: bool) -> Self {
        self.compile_only = compile_only;
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

    /// What the run says of the tree's `trust.lock` ([`lock_notes`]).
    #[must_use]
    pub fn with_lock_notes(mut self, notes: Vec<String>) -> Self {
        self.lock_notes = notes;
        self
    }

    /// HEAD when the run's selection was read ([`Ctx::head_at_start`]).
    #[must_use]
    pub fn with_head_at_start(mut self, head: Option<String>) -> Self {
        self.head_at_start = head;
        self
    }

    /// Carry the complete test proofs `passed` names when their current executable and runtime
    /// inputs still match ([`Ctx::passed_binaries`]).
    #[must_use]
    pub fn with_passed_binaries(mut self, passed: Option<testrun::Passed>) -> Self {
        self.passed_binaries = passed;
        self
    }

    /// Carry prior per-package lint successes into a release lint run.
    #[must_use]
    pub fn with_passed_lint_packages(mut self, passed: Option<lint_content::Passed>) -> Self {
        self.passed_lint_packages = passed;
        self
    }

    /// Carry prior AppKit group successes when their current executable bytes still match.
    #[must_use]
    pub fn with_passed_appkit_groups(mut self, passed: Option<appkit_content::Passed>) -> Self {
        self.passed_appkit_groups = passed;
        self
    }

    /// Salt test evidence with its frozen caller context, or release lint with its frozen
    /// verifier, seal and toolchain context.
    #[must_use]
    pub fn with_carry_context(mut self, context: Option<String>) -> Self {
        self.carry_context = context;
        self
    }

    /// Bracket each development test binary's inputs ([`Ctx::input_drift`]).
    #[must_use]
    pub fn with_input_drift(mut self, input_drift: bool) -> Self {
        self.input_drift = input_drift;
        self
    }

    /// Write each check, binary and observed case event to `events` too
    /// ([`Ctx::events`]).
    #[must_use]
    pub fn with_events(mut self, events: Option<event_log::EventLog>) -> Self {
        self.events = events;
        self
    }

    /// Write `line`, a `verify-event: ` line, to the run's events file, when it has one.
    pub fn record_event(&self, line: &str) -> String {
        if let Some(events) = &self.events {
            let line = events.prepare(line);
            events.write_prepared(&line);
            line
        } else {
            line.to_owned()
        }
    }

    /// Keep each test binary's run in the history at `path` ([`Ctx::test_history`]).
    #[must_use]
    pub fn with_test_history(mut self, path: Option<PathBuf>) -> Self {
        self.test_history = path;
        self
    }

    /// Measure each stage's memory and keep its peak in `book` ([`Ctx::memory`]). Each stage's
    /// memory claim covers the builds its children start ([`aterm_capacity::Need::covers`]): a
    /// stage's children are handed its own claim's cover marker ([`aterm_capacity::COVER_ENV`],
    /// [`memory::cover`]), so a build a test runs that would claim (`atpkg::build_admit`) claims
    /// nothing more while its stage's claim is held, and every other child of the run is handed
    /// [`aterm_capacity::NO_COVER`], so a stage that claimed nothing never borrows another's.
    #[must_use]
    pub fn with_memory(mut self, book: memory::Book) -> Self {
        self.memory = Some(book);
        self.child_env_add.push((
            aterm_capacity::COVER_ENV.into(),
            aterm_capacity::NO_COVER.into(),
        ));
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
            events: self.events.as_ref(),
            cwd: &self.root,
            path: &self.path_env,
            scratch: &self.scratch,
            child_ceiling: self.child_ceiling,
            remove_env: &self.child_env_remove,
            add_env: &self.child_env_add,
            stage_groups: self.runs.as_ref().map(|r| r as &dyn exec::StageGroupEnd),
            host: &exec::LIVE_HOST,
            hang_samples: self.hang_samples.as_deref(),
        }
    }

    /// The command that reruns `check` over this run's selection, as this run ran it: a release's
    /// `test` and `doc` rows name `--as-release`, without which they carry no
    /// `ATERM_SPEC_CHECKERS=1` and `test` owes no [`checks::GOLDEN`] test, so would pass what
    /// failed. A cone's `lint` row names the cone's base: `-p` over its crates would lint every
    /// target of each and the crates it left out ([`stages::tippy_passes`]). So does a cone's
    /// `export-scan` row: under `-p` it scans the whole export, not the changed files. A lint rerun pulls
    /// `cells` as any dev lint does ([`checks::pulls_cells`]).
    #[must_use]
    pub fn rerun(&self, check: Check) -> String {
        let mut cmd = format!("targo --unverified verify {}", check.name());
        if check.reads(&self.scope) {
            match &self.scope {
                Scope::Workspace => cmd.push_str(" --all"),
                s if s.selects_nothing() => {}
                Scope::Changed(cone) if matches!(check, Check::Lint | Check::ExportScan) => {
                    cmd.push_str(" --base ");
                    cmd.push_str(&cone.base);
                }
                s => {
                    for c in s.crates() {
                        cmd.push_str(" -p ");
                        cmd.push_str(c);
                    }
                }
            }
        }
        if self.cursor_pacing && check == Check::Measure {
            cmd.push_str(" --cursor-pacing");
        }
        if self.compile_only && matches!(check, Check::Test | Check::Checkers | Check::Measure) {
            cmd.push_str(" --compile-only");
        }
        if self.checkers_in_tests && matches!(check, Check::Test | Check::Doc) {
            cmd.push_str(" --as-release");
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

/// How a claim in this user's capacity ledger is admitted, a timed stage's quiet claim or a
/// stage's memory claim: [`aterm_capacity::admit`], or a test's.
pub(crate) type Admitter = fn(
    aterm_capacity::Need,
    aterm_capacity::Admit,
) -> Result<aterm_capacity::Claim, aterm_capacity::Error>;

/// What runs a stage, admits the run's quiet machine and admits each stage's memory: the live
/// ones ([`LIVE`]), or a test's stand-ins.
#[derive(Clone, Copy)]
struct Seams {
    stage: fn(&Ctx, &plan::StageSpec) -> Report,
    prepare_build_dir: fn(&Ctx) -> std::io::Result<()>,
    prepare_cells: Option<fn(&Ctx) -> Option<cells::PreparedRun>>,
    admit_quiet: Admitter,
    admit_memory: Admitter,
}

#[cfg(test)]
#[path = "stage_diagnostic_tests.rs"]
mod stage_diagnostic_tests;

/// The stages themselves, and the per-user capacity ledger on the live machine.
const LIVE: Seams = Seams {
    stage: stages::run_stage,
    prepare_build_dir: prepare_main_build_dir,
    prepare_cells: Some(stages::prepare_cells),
    admit_quiet: admit_live,
    admit_memory: admit_live,
};

/// Admit `need` in this user's ledger on the live machine ([`admit_in`]), saying on stderr what
/// it waits for.
///
/// # Errors
/// As [`admit_in`]; [`aterm_capacity::Error::Ledger`] with no home to keep the ledger under.
pub(crate) fn admit_live(
    need: aterm_capacity::Need,
    mode: aterm_capacity::Admit,
) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
    let ledger = aterm_capacity::Ledger::user().ok_or_else(|| {
        aterm_capacity::Error::Ledger(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no home directory to keep the ledger under",
        ))
    })?;
    admit_in(
        &ledger,
        &aterm_capacity::Live,
        aterm_capacity::Pace::default(),
        need,
        mode,
        &mut |line| eprintln!("{line}"),
    )
}

/// Admit `need` in `ledger` on `machine` as [`aterm_capacity::admit_with`] does, saying through
/// `say` what it waits for, its wait ended by the run's end too ([`stopping`]): one interrupt
/// reaches one of the run's waits (`aterm_toolchain::interrupt::Watch::take`), which ends the run
/// (`exec::group::stop_run`), and every other wait of its stages, a quiet claim's or another
/// stage's memory claim's, ends at its next look. A need the run asks to end behind holders that
/// make no progress ([`STUCK_ENDS`]) ends there too, that stage alone. Its first line says what
/// the memory claim this thread admits claims and why ([`claiming`]), so a wait behind other
/// claims names where its figure came from.
///
/// # Errors
/// As [`aterm_capacity::admit_until`]: [`aterm_capacity::Error::Stopped`] once the run ends, and
/// [`aterm_capacity::Error::Stuck`] behind holders that made no progress. A cohort ledger or
/// failed namespace validation returns [`aterm_capacity::Error::Ledger`]: this verifier
/// still uses holder-based claims and cannot supervise cohort custody.
fn admit_in(
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    pace: aterm_capacity::Pace,
    need: aterm_capacity::Need,
    mode: aterm_capacity::Admit,
    say: &mut dyn FnMut(&str),
) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
    match ledger.cohort_aware() {
        Ok(false) => {}
        Ok(true) => {
            return Err(aterm_capacity::Error::Ledger(std::io::Error::other(
                "the holder-based verifier cannot supervise a cohort ledger; native cohort launch is required",
            )));
        }
        Err(error) => return Err(aterm_capacity::Error::Ledger(error)),
    }
    let mut why = CLAIMING.with(|c| c.borrow().clone());
    aterm_capacity::admit_until(
        ledger,
        machine,
        need,
        mode,
        pace,
        &mut |line| match why.take() {
            Some(why) => say(&format!("{line}; it claims {why}")),
            None => say(line),
        },
        &stopping,
    )
}

/// A whole-tree Background candidate's disk is Background too. It may finish its current stage;
/// Ordinary work retains priority for overlapping resources, and a Release may end it immediately.
const fn run_admit_mode(
    background: bool,
    requested: aterm_capacity::Admit,
) -> aterm_capacity::Admit {
    if background {
        aterm_capacity::Admit::Preemptible
    } else {
        requested
    }
}

thread_local! {
    /// What the memory claim this thread admits claims and why ([`claiming`]).
    static CLAIMING: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// This thread's memory claim says `why` in its first wait line ([`admit_in`]) until the answer
/// drops, when the one before comes back.
#[must_use = "the reason is this thread's only while the guard lives"]
struct Claiming {
    before: Option<String>,
    /// A TLS restoration guard stays on the thread where it was installed.
    _thread: std::marker::PhantomData<*mut ()>,
}

/// Make `why` what this thread's memory claim says it claims ([`Claiming`]).
fn claiming(why: String) -> Claiming {
    Claiming {
        before: CLAIMING.with(|c| c.replace(Some(why))),
        _thread: std::marker::PhantomData,
    }
}

impl Drop for Claiming {
    fn drop(&mut self) {
        let before = self.before.take();
        CLAIMING.with(|c| *c.borrow_mut() = before);
    }
}

/// How long the holders a stage's quiet or memory claim waits behind, and the run's disk claim,
/// may make no progress before the wait gives up ([`aterm_capacity::Need::ends_when_stuck`],
/// CW-WAIT-UNBOUNDED): that stage is then NOT RUN, or the run COULD NOT RUN for its disk, naming
/// them, and nobody signals them. The stage's own children waiting behind one are ended on the
/// same bound (`exec::Clock::stuck`).
pub(crate) const STUCK_ENDS: Duration = aterm_toolchain::stall::STALLED_AFTER;

/// The run's cached stage reservation ([`Ctx::quiet`]), shared while its bounded hold remains
/// valid. A later stage freshly admits a replacement after expiry; the old record remains
/// owned by any earlier stage or bounded worker until that work ends. Each measurement
/// collector separately establishes its own postcompile entry evidence and protection.
struct Quiet {
    /// The currently cached claim, once admitted; old workers can retain earlier claims.
    claim: Option<Arc<aterm_capacity::Claim>>,
    /// The run's timed stages still to end ([`takes_quiet`]).
    left: usize,
    /// How the claim is admitted.
    admit: Admitter,
}

impl Quiet {
    /// Room for the timed stages of `stages` that `ctx` runs, admitted by `admit`.
    fn of(ctx: &Ctx, stages: &[plan::StageSpec], admit: Admitter) -> Mutex<Quiet> {
        Mutex::new(Quiet {
            claim: None,
            left: stages.iter().filter(|s| takes_quiet(ctx, s.id)).count(),
            admit,
        })
    }

    /// One timed stage ended, or never ran: the run's share goes with the last. A worker that
    /// outlived its stage still holds its own share until it ends.
    fn passed(&mut self) {
        self.left = self.left.saturating_sub(1);
        if self.left == 0 {
            self.claim = None;
        }
    }
}

/// One started timed stage's turn in the run's [`Quiet`]: dropped, on return or on a panic's
/// unwind ([`sched::run_stages`] re-raises a stage's panic), it calls [`Quiet::passed`], so a
/// stage that panics never leaves the run's quiet or window claim held behind it until every
/// other running stage ends.
#[must_use = "the stage's turn passes when the guard drops"]
struct Passes<'a>(&'a Mutex<Quiet>);

impl Drop for Passes<'_> {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .passed();
    }
}

thread_local! {
    /// The quiet reservation of this stage or bounded worker.
    static QUIET_CLAIM: RefCell<Option<Arc<aterm_capacity::Claim>>> = const { RefCell::new(None) };
    /// The actual stage resource reservation retained by every supported worker handoff.
    static RESOURCE_CLAIM: RefCell<Option<Arc<aterm_capacity::Claim>>> = const { RefCell::new(None) };
}

/// The quiet reservation of this thread, captured before a bounded worker starts.
fn current_quiet_claim() -> Option<Arc<aterm_capacity::Claim>> {
    QUIET_CLAIM.with(|claim| claim.borrow().clone())
}

type ClaimSlot = std::thread::LocalKey<RefCell<Option<Arc<aterm_capacity::Claim>>>>;

/// Restore this thread's previous reservation when a stage or worker scope ends.
#[must_use = "the reservation is this thread's only while the guard lives"]
struct ReservationScope {
    slot: &'static ClaimSlot,
    before: Option<Arc<aterm_capacity::Claim>>,
    /// A TLS restoration guard stays on the thread where it was installed.
    _thread: std::marker::PhantomData<*mut ()>,
}

/// Hold `claim` as this thread's quiet reservation, restoring its predecessor on drop. A
/// worker gets an owned share so a stage timeout cannot release its still-running work's desk.
fn holding_quiet_claim(claim: Option<Arc<aterm_capacity::Claim>>) -> ReservationScope {
    holding_claim(&QUIET_CLAIM, claim)
}

fn current_resource_claim() -> Option<Arc<aterm_capacity::Claim>> {
    RESOURCE_CLAIM.with(|claim| claim.borrow().clone())
}

fn holding_resource_claim(claim: Option<Arc<aterm_capacity::Claim>>) -> ReservationScope {
    holding_claim(&RESOURCE_CLAIM, claim)
}

fn holding_claim(
    slot: &'static ClaimSlot,
    claim: Option<Arc<aterm_capacity::Claim>>,
) -> ReservationScope {
    ReservationScope {
        slot,
        before: slot.with(|current| current.replace(claim)),
        _thread: std::marker::PhantomData,
    }
}

impl Drop for ReservationScope {
    fn drop(&mut self) {
        let before = self.before.take();
        self.slot.with(|claim| *claim.borrow_mut() = before);
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

/// What a proven red ends ([`Ctx::halt`]): the tree half collects the complete sweep unless
/// `--fail-fast` is named. Other releases and explicitly fail-fast runs stop at every red;
/// ordinary runs stop only at source/type preflight failures, retaining the test sweep
/// required by `verify land`'s inheritance rule.
fn halt_of(args: &cli::Args) -> sched::Halt {
    #[cfg(unix)]
    let end: fn(&str) = exec::group::halt;
    #[cfg(not(unix))]
    let end: fn(&str) = |_| {};
    if args.release && args.tree_only && !args.fail_fast {
        sched::Halt::off()
    } else if args.release {
        sched::Halt::at_first_red("the release", end)
    } else if args.fail_fast {
        sched::Halt::at_first_red("the run", end)
    } else {
        sched::Halt::after_preflight(end)
    }
}

/// End the run at a proven red ([`Ctx::halt`]) once `report`, of a stage `check` owns, holds a
/// FAIL row: a stage of several children calls it between them, so its first red ends the run
/// without waiting for the rest. A NOT RUN row never does, nor a `measure` red: the owner may
/// accept a measure failure once every other release check passed (`aterm-release`'s accepted
/// measure), so that check runs whole and every other check's verdict stands beside it.
pub(crate) fn red_now(ctx: &Ctx, check: Check, report: &Report) {
    if check != Check::Measure
        && report
            .outcomes()
            .any(|(o, _)| o == Outcome::Fail(Severity::GateFailed))
    {
        ctx.halt.red(check.name());
    }
}

/// Why a stage an interrupt kept from starting did not run.
const ENDING: &str = "the run was interrupted before this stage started, so it decided nothing";

/// Why the run's end keeps a stage from starting now, if it does: the halt's words once its first
/// proven red ended it ([`Ctx::halted`]), else [`ENDING`] once an interrupt is ending it
/// ([`stopping`]).
fn ended(ctx: &Ctx) -> Option<String> {
    ctx.halted()
        .map(str::to_owned)
        .or_else(|| stopping().then(|| ENDING.to_owned()))
}

/// What the run's quiet claim did for a timed stage, for its time line (or a browser session's
/// own, for the session that takes the desk): `first` when the stage's admission made it. A
/// claim whose patience ran out says it started at its bound, under what load, and how long it
/// reserved the machine ([`aterm_capacity::QuietStart`]).
pub(crate) fn admission(
    claim: &aterm_capacity::Claim,
    mode: aterm_capacity::Admit,
    first: bool,
) -> String {
    let bound = claim.record().need.quiet.unwrap_or_default();
    if claim.record().need.quiet_policy == aterm_capacity::QuietPolicy::Timing {
        // How busy the GPU read at admission, which it waited for until its bound (QUIET-BURST).
        let gpu = claim
            .quiet()
            .and_then(|q| q.gpu)
            .map_or_else(String::new, |g| format!(", GPU {g}% busy"));
        if let Ok(aterm_capacity::Protection::Loaded(observed)) = claim.timing_protection() {
            return format!(
                "{} loaded timing reservation after {}: {}{gpu}; ran without a qualified quiet \
                 admission (stated, not a finding: a busy machine never blocks a release)",
                if first { "acquired" } else { "retained" },
                aterm_capacity::span(claim.waited()),
                aterm_capacity::Unqualified::Admission(observed),
            );
        }
        let evidence = claim.record().timing;
        return evidence.admission.map_or_else(
            || "timing reservation with unknown admission CPU; no measurement qualification".into(),
            |window|format!("{} timing reservation; admission CPU {:.2}% over {:.3}s at epoch {}ms, load {:?} <= {bound:.1}{gpu}; admission sample is not fresh measurement-entry evidence or continuous quiet",
                if first {"acquired"}else{"retained"},window.busy_ticks as f64*100.0/window.total_ticks as f64,window.interval_ns as f64/1e9,window.observed_ms,evidence.admission_load1),
        );
    }
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
        aterm_capacity::Admit::Wait | aterm_capacity::Admit::Preemptible => {
            format!("a quiet machine (load <= {bound:.1})")
        }
    }
}

/// Run a timed stage on the run's quiet machine ([`Quiet`]): the first one's claim is admitted
/// before it starts, waiting for the load within its patience ([`aterm_capacity::QUIET_PATIENCE`])
/// and saying once a minute what it waits for, and let go when the last one's `f` and any bounded
/// worker it left running end. A machine that stays busy through that patience admits it loaded
/// ([`aterm_capacity::AtBound::RunLoaded`]): the stage runs and its time line says so (owner
/// ruling 2026-11-05: a busy machine never blocks a release). Unavailable, blind, forced or
/// expired timing admission starts no callback. A later stage replaces an expired cached reservation through fresh admission;
/// existing stage and bounded-worker shares retain ownership of their original claim record,
/// without extending its expired protection.
/// Measurement collectors establish their own fresh entry after compilation;
/// this retained stage reservation is resource exclusion, not fresh CPU evidence.
/// Also what the claim did, for the stage's time line.
///
/// # Errors
/// Why `f` never ran: an interrupt ended the wait, which ends the run
/// (`exec::group::stop_run`: no stage child starts after it, and each live one is ended, so the
/// stages it cut short or kept from starting are COULD NOT RUN); the wait gave up behind holders
/// that made no progress ([`STUCK_ENDS`]), and this stage alone did not run; or the run was
/// already ending ([`stopping`]), and no wait begins.
fn quietly<T>(
    ctx: &Ctx,
    quiet: &Mutex<Quiet>,
    title: &str,
    f: impl FnOnce() -> T,
) -> Result<(T, Option<String>), String> {
    quietly_with(
        ctx,
        quiet,
        title,
        f,
        aterm_capacity::Claim::timing_protection,
        |q, need, mode| (q.admit)(need, mode),
    )
}

fn quietly_with<T>(
    ctx: &Ctx,
    quiet: &Mutex<Quiet>,
    title: &str,
    f: impl FnOnce() -> T,
    check: impl Fn(
        &aterm_capacity::Claim,
    ) -> Result<aterm_capacity::Protection, aterm_capacity::TimingIssue>,
    mut admit: impl FnMut(
        &Quiet,
        aterm_capacity::Need,
        aterm_capacity::Admit,
    ) -> Result<aterm_capacity::Claim, aterm_capacity::Error>,
) -> Result<(T, Option<String>), String> {
    let Some(mode) = ctx.quiet else {
        return match ended(ctx) {
            Some(why) => Err(why),
            None => Ok((f(), None)),
        };
    };
    let mut q = quiet.lock().unwrap_or_else(PoisonError::into_inner);
    // Expiry ends qualification, not an existing worker's ownership. Keep this local share
    // through replacement admission; TLS and bounded workers retain their own old shares.
    // Only expiry permits replacement, never unknown/forced provenance or another failure.
    let checked = q.claim.as_ref().map(|c| check(c));
    let retired = if matches!(checked, Some(Err(aterm_capacity::TimingIssue::Expired))) {
        q.claim.take()
    } else {
        None
    };
    let said = if ended(ctx).is_some() {
        Err(None)
    } else {
        match &q.claim {
            Some(claim) => {
                match checked.unwrap_or(Err(aterm_capacity::TimingIssue::UnknownAdmission)) {
                    Ok(_) => Ok(admission(claim, mode, false)),
                    Err(why) => {
                        q.passed();
                        return Err(format!(
                            "{title}: timing reservation cannot qualify this stage: {why}; started nothing"
                        ));
                    }
                }
            }
            None => match admit(
                &q,
                aterm_capacity::Need::timing(format!("verify {title}"))
                    .runs_loaded()
                    .ends_when_stuck(STUCK_ENDS),
                mode,
            ) {
                Ok(claim) => {
                    if let Err(why) = check(&claim) {
                        q.passed();
                        return Err(format!(
                            "{title}: unqualified timing admission: {why}; started nothing"
                        ));
                    }
                    let said = admission(&claim, mode, true);
                    q.claim = Some(Arc::new(claim));
                    Ok(said)
                }
                Err(
                    e @ (aterm_capacity::Error::Stopped { .. }
                    | aterm_capacity::Error::Stuck { .. }),
                ) => Err(Some(e)),
                Err(e) => {
                    q.passed();
                    return Err(format!(
                        "{title}: timing admission unavailable: {e}; started nothing"
                    ));
                }
            },
        }
    };
    drop(retired);
    let said = match said {
        Ok(said) => said,
        Err(stopped) => {
            q.passed();
            drop(q);
            let Some(e) = stopped else {
                return Err(ended(ctx).unwrap_or_else(|| ENDING.to_owned()));
            };
            if let Some(words) = ctx.halted() {
                return Err(words.to_owned());
            }
            if matches!(e, aterm_capacity::Error::Stuck { .. }) {
                eprintln!("verify: {title}: the run's quiet claim {e}; {title} decided nothing");
                return Err(format!("its quiet claim {e}; it started nothing"));
            }
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
    let claim = q.claim.clone();
    drop(q);
    let _quiet = holding_quiet_claim(claim);
    let _passes = Passes(quiet);
    Ok((f(), Some(said)))
}

/// The ` time ` line under a stage in the log: how long it ran, when it started, and `machine`,
/// what the machine did meanwhile ([`aterm_capacity::between`]), how a timed stage was admitted
/// and what memory the stage waited for and peaked at.
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

/// Whether stage `id` runs on the quiet machine: a timed stage ([`plan::TIMED`]), or the browser
/// lane where a session of it takes the desk ([`browser::takes_the_desk`]: Safari's window), that
/// runs, not a named skip before it starts anything ([`plan::named_skip`]: a windowed stage under
/// `--no-windows`, a measuring one with no `--app`).
fn takes_quiet(ctx: &Ctx, id: plan::StageId) -> bool {
    let desk = id == plan::StageId::BrowserLane && browser::takes_the_desk(ctx);
    (id.timed() || desk) && !plan::named_skip(ctx, id)
}

const APPKIT_NOW: &str = "ran immediately under --admit now without quiet qualification; live \
load is reported, not a refusal";

/// Make one non-overriding attempt to own the per-user window admission on the live machine.
///
/// # Errors
/// The capacity ledger is unavailable.
fn try_window_live(
    need: aterm_capacity::Need,
) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error> {
    let ledger = aterm_capacity::Ledger::user().ok_or_else(|| {
        aterm_capacity::Error::Ledger(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no home directory to keep the ledger under",
        ))
    })?;
    aterm_capacity::admit::try_admit_with(&ledger, &aterm_capacity::Live, need)
}

/// Run `f` under the timing rule for `id`. AppKit's live correctness drivers have no timing
/// verdict: an explicit immediate release gate must execute them under the load it finds, while
/// the machine readings around the stage retain that load in its time line. Measurements still
/// require qualified timing admission and therefore fail closed under `--admit now`.
fn run_timed<T>(
    ctx: &Ctx,
    quiet: &Mutex<Quiet>,
    id: plan::StageId,
    title: &str,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<(T, Option<String>), String> {
    run_timed_with(ctx, quiet, id, title, try_window_live, f)
}

fn run_timed_with<T>(
    ctx: &Ctx,
    quiet: &Mutex<Quiet>,
    id: plan::StageId,
    title: &str,
    mut try_window: impl FnMut(
        aterm_capacity::Need,
    ) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error>,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<(T, Option<String>), String> {
    if !takes_quiet(ctx, id) {
        return f().map(|out| (out, None));
    }
    if ctx.quiet == Some(aterm_capacity::Admit::Now)
        && matches!(
            id,
            plan::StageId::RedrawConformance | plan::StageId::ObjcDrives
        )
    {
        let mut q = quiet.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(why) = ended(ctx) {
            q.passed();
            return Err(why);
        }
        if q.claim.is_none() {
            let need = aterm_capacity::Need::new(format!("verify {title}"))
                // Infinity ignores load while retaining the ledger's cross-process exclusion:
                // another quiet/window claim is still a shortfall. No patience means no GPU or
                // timing wait, and `try_admit_with` makes exactly one non-overriding look.
                .quiet(f64::INFINITY)
                .checkout(ctx.root.to_string_lossy());
            match try_window(need) {
                Ok(Some(claim)) => q.claim = Some(Arc::new(claim)),
                Ok(None) => {
                    q.passed();
                    return Err(format!(
                        "{title}: the per-user window admission was unavailable at its immediate \
                         look; started nothing"
                    ));
                }
                Err(error) => {
                    q.passed();
                    return Err(format!(
                        "{title}: the per-user window admission is unavailable: {error}; started \
                         nothing"
                    ));
                }
            }
        }
        let claim = q.claim.clone();
        drop(q);
        let _quiet = holding_quiet_claim(claim);
        let _passes = Passes(quiet);
        return f().map(|out| (out, Some(APPKIT_NOW.to_owned())));
    }
    quietly(ctx, quiet, title, f).and_then(|(out, admitted)| Ok((out?, admitted)))
}

/// What stage `spec`'s memory did, for its time line, what it held read off `meter`
/// ([`memory::Meter::held`]: 0 when it started no child) kept in the run's book
/// ([`memory::Book::keep`]) when it is what the stage takes: only when a row of it passed and
/// none failed, since a stage that failed or could not run may have stopped short of its peak (a
/// compile error in its fifth crate) and one whose every row was skipped did none of its work;
/// and a 0 only when no row was skipped either, since the skipped row may be the one that starts
/// a child. A figure not kept is said. `None` when nothing was measured: no book, a stage that is
/// not metered, or children no read booked.
fn measured(
    ctx: &Ctx,
    spec: &plan::StageSpec,
    report: &Report,
    meter: Option<&memory::Meter>,
    compile_jobs: Option<u32>,
    units: Option<&compile_width::Units>,
) -> Option<String> {
    let book = ctx.memory.as_ref()?;
    if !memory::metered(spec) {
        return None;
    }
    let meter = meter?;
    let peak = meter.held()?;
    let size = meter.peak().map_or_else(
        || "nothing: it started no child".to_owned(),
        aterm_capacity::size,
    );
    let rows: Vec<Outcome> = report.outcomes().map(|(o, _)| o).collect();
    let not_kept = if rows.iter().any(|o| matches!(o, Outcome::Fail(_))) {
        Some("a stage that did not pass may have stopped short of its peak")
    } else if !rows.contains(&Outcome::Ok) {
        Some("no row of it passed, so it did none of its work")
    } else if peak == 0 && rows.contains(&Outcome::Skip) {
        Some("it skipped a row, which may be the one that starts a child")
    } else {
        None
    };
    if let Some(why) = not_kept {
        return Some(format!("memory: peaked {size}, not kept: {why}"));
    }
    let key = compile_width::record_key(memory::Key::of_stage(spec, ctx), compile_jobs);
    let mut said = match book.keep(spec, &key, peak) {
        Ok(memory::Kept::Kept | memory::Kept::Nowhere) => format!("memory: peaked {size}"),
        Ok(memory::Kept::Light) => format!(
            "memory: peaked {size}, not kept: light beside the heaviest peak its record kept in \
             two weeks, which stands"
        ),
        Ok(memory::Kept::LightBeside(by)) => format!(
            "memory: peaked {size}, not kept: below a quarter of the {} its claim was planned \
             by, so it compiled almost none of its work",
            aterm_capacity::size(by)
        ),
        Err(e) => format!("memory: peaked {size}, not kept: {e}"),
    };
    // The shaping book remains meter-derived and independent of release pooling. It retains every
    // compiler the meter identified, raising a light one to the meter's floor, because a later
    // plan may sum unit peaks only when its history covers every named unit.
    let unit_peaks = (spec.id == plan::StageId::TestCompile).then(|| meter.unit_claim_peaks());
    if let Some(unit_peaks) = &unit_peaks
        && let Err(error) = book.keep_units(&key.checkout, unit_peaks)
    {
        said.push_str(&format!("; test compiler shapes not kept: {error}"));
    }
    if let (Some(units), Some(unit_peaks)) = (units, &unit_peaks) {
        match compile_width::keep_meter_units(book, spec.id, units, unit_peaks) {
            Ok(kept) if kept > 0 => said.push_str(&format!(
                "; {kept} compiler footprint peak{} merged into its unit book",
                if kept == 1 { "" } else { "s" }
            )),
            Ok(_) => {}
            Err(error) => said.push_str(&format!("; compiler footprint peaks not kept: {error}")),
        }
    }
    Some(said)
}

/// What the run's memory budget of `budget` bytes held stage `i` of `stages` back for
/// ([`sched::Held`]), for its time line: `memory: held 41.2s for room behind tippy lint (-p
/// aterm-gui): it claims 12 GiB of the run's 21.5 GiB`; `None` when it was not held.
fn held_note(
    stages: &[plan::StageSpec],
    i: usize,
    held: &sched::Held,
    budget: u64,
) -> Option<String> {
    if held.behind.is_empty() {
        return None;
    }
    let behind: Vec<&str> = held
        .behind
        .iter()
        .filter_map(|&j| stages.get(j).map(|s| s.title.as_str()))
        .collect();
    Some(format!(
        "memory: held {:.1}s for room behind {}: it claims {} of the run's {}",
        held.waited.as_secs_f64(),
        behind.join(", "),
        aterm_capacity::size(stages.get(i).map_or(0, |s| s.mem)),
        aterm_capacity::size(budget),
    ))
}

/// A stage's resource claim in this user's capacity ledger ([`claim_memory`]), or a test run's
/// for its binaries ([`claim_test_resources`]), held until the stage and its supported workers
/// end, and what it did for the stage's time line. Ordinary stages ask only for memory;
/// repeatable Background stages ask for memory and CPU together.
pub(crate) struct ResourceClaim {
    /// The actual admitted claim, shared with supported workers until they finish.
    pub(crate) claim: Arc<aterm_capacity::Claim>,
    /// What it claimed, why, and how it was admitted.
    pub(crate) said: String,
    /// Unit admission configured by the early claim, installed only on the stage's own thread.
    pub(crate) units: Option<compile_width::Units>,
}

impl ResourceClaim {
    /// The cover marker its stage's children are handed ([`memory::cover`]).
    pub(crate) fn cover(&self) -> Option<std::sync::Arc<str>> {
        (self.claim.record().need.mem > 0)
            .then(|| std::sync::Arc::from(self.claim.record().cover_marker()))
    }
}

/// Resource claims a pooled release run takes before any stage starts. The qualify graph holds
/// its CPU tokens until the build has reserved its slice memory and this verifier has reserved
/// lint's whole-stage memory. Hold that reservation until lint starts, then transfer it into
/// the stage without a second claim.
#[derive(Default)]
struct EarlyClaims {
    claim: Mutex<Option<(plan::StageId, Option<ResourceClaim>)>>,
}

impl EarlyClaims {
    /// Pre-admit lint for a release running inside the qualify pool. An ordinary verifier, a
    /// release with no lint stage, and an unpooled release retain stage-time admission.
    fn for_run(
        ctx: &Ctx,
        stages: &[plan::StageSpec],
        admit: Admitter,
    ) -> Result<EarlyClaims, String> {
        let claim = (ctx.release && ctx.pool.is_some() && ctx.scope.is_workspace())
            .then(|| stages.iter().find(|stage| stage.id == plan::StageId::Tippy))
            .flatten()
            .map(|stage| {
                // Branded Tippy deliberately excludes outer compiler wrappers, so its retained
                // whole-stage history is the only enforceable memory envelope. Its pooled claim
                // is that measured peak, or the machine's memory alone where none is kept
                // (`compile_width::lint_claim`).
                let early = stage.clone();
                before_stage_claim(ctx)
                    .and_then(|()| claim_memory(ctx, &early, admit))
                    .map(|claim| (stage.id, claim))
            })
            .transpose()?;
        Ok(EarlyClaims {
            claim: Mutex::new(claim),
        })
    }

    /// Take `stage`'s pre-admitted claim exactly once. A different stage leaves it in place.
    fn take(&self, stage: plan::StageId) -> Option<Option<ResourceClaim>> {
        let mut claim = self
            .claim
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if claim.as_ref().is_some_and(|(id, _)| *id == stage) {
            claim.take().map(|(_, claim)| claim)
        } else {
            None
        }
    }
}

/// The CPU width a repeatable tree stage asks for. A caller's smaller explicit build ceiling is
/// retained; otherwise the host's parallelism is the upper bound. Capacity may grant less.
fn background_cores(ctx: &Ctx) -> Option<u32> {
    ctx.background.then(|| {
        ctx.build_jobs_ceiling()
            .unwrap_or_else(|| ctx.parallelism())
            .max(1)
    })
}

/// How `claim`, admitted as `mode` says, came to be admitted, for its stage's time line: by
/// `--admit now`; after how long a wait for what (behind which claims, for the memory pressure to
/// ease, or for memory no claim holds); and alone ([`aterm_capacity::Alone`]). Empty for a claim
/// admitted beside the others at its first look.
fn admitted_how(claim: &aterm_capacity::Claim, mode: aterm_capacity::Admit) -> String {
    let mut how = match mode {
        aterm_capacity::Admit::Now => ", admitted by --admit now".to_owned(),
        aterm_capacity::Admit::Wait if claim.waited() >= Duration::from_secs(1) => {
            let what = match claim.waited_for() {
                Some(short) if !short.holders.is_empty() => format!(
                    "behind {}",
                    short
                        .holders
                        .iter()
                        .map(|h| format!("pid {} ({:?})", h.pid, h.what))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Some(short) if short.shorts.contains(&aterm_capacity::Short::Pressure) => {
                    "for the memory pressure to ease".to_owned()
                }
                _ => "for memory no claim holds".to_owned(),
            };
            format!(
                ", after {} waiting {what}",
                aterm_capacity::span(claim.waited())
            )
        }
        aterm_capacity::Admit::Wait | aterm_capacity::Admit::Preemptible => String::new(),
    };
    if let Some(alone) = claim.alone() {
        how.push_str(", ");
        how.push_str(&alone.to_string());
    }
    how
}

/// Claim stage `spec`'s memory ([`plan::StageSpec::mem`]) in this user's capacity ledger before
/// it starts, admitted by `admit` as `--admit` says ([`Ctx::quiet`], wait unless it says now):
/// waiting while other runs' claims and what is available leave no room for it, and saying once a
/// minute what it waits for (the first line what it claims and why, as its time line says it:
/// [`admit_in`]), until every claim it waits behind is held by a holder that has made
/// no progress for [`STUCK_ENDS`]; at once under `--admit now`. Short only of
/// memory no claim holds (other work on the machine), it is admitted alone, as the ledger's ALONE
/// rule says, and no other memory claim is admitted while it runs. A stage that claims more than
/// can ever be available here ([`memory::Book::budget`]), which the ledger would refuse, asks to
/// run alone ([`aterm_capacity::Need::alone`]): it waits for every other memory claim to end and
/// then runs with none beside it, in this run ([`sched::memory_holders`]) or another. One the
/// ledger refuses at its look though the run's budget held it (the machine read now has more
/// wired) asks again, alone. Failed admission starts no stage work. `Ok(None)` for a stage
/// that claims nothing, or a run with no book.
///
/// # Errors
/// Why the stage never ran: an interrupt ended the wait, which ends the run
/// (`exec::group::stop_run`, as a quiet claim's does in [`quietly`]); the wait gave up behind
/// holders that made no progress, naming them, and this stage alone did not run; or the run was
/// already ending ([`stopping`]), and no wait begins; or admission fails because the ledger
/// cannot be used, required timing is unavailable, or the need cannot be admitted alone.
fn claim_memory(
    ctx: &Ctx,
    spec: &plan::StageSpec,
    admit: Admitter,
) -> Result<Option<ResourceClaim>, String> {
    if let Some(claim) = compile_width::claim(ctx, spec, admit)? {
        return Ok(Some(claim));
    }
    let Some(book) = ctx.memory.as_ref() else {
        return Ok(None);
    };
    let cores = background_cores(ctx).unwrap_or(0);
    if spec.mem == 0 {
        if cores == 0 || memory::dispatched(spec.id) {
            return Ok(None);
        }
        return claim_bytes(
            ctx,
            &spec.title,
            0,
            "its CPU share",
            book.budget(),
            cores,
            admit,
        )
        .map(Some);
    }
    let need = book.need(
        spec,
        &memory::Key::of_stage(spec, ctx),
        compile_width::jobs_for(ctx, spec.id),
    );
    let why = match &spec.part {
        Some(part) => format!("{} {}", aterm_capacity::size(spec.mem), part.said),
        None if need.bytes == spec.mem => need.to_string(),
        None => aterm_capacity::size(spec.mem),
    };
    claim_bytes(
        ctx,
        &spec.title,
        spec.mem,
        &why,
        book.budget(),
        cores,
        admit,
    )
    .map(Some)
}

/// Claim `bytes` for a test run's binaries (`what` says which: the most its plan's hold at once,
/// [`test_plan::schedule`], or the recording pass), from their history, as a stage's
/// memory is claimed ([`claim_memory`]), admitted by `admit`: the test run's stage claims none of
/// its own ([`memory::dispatched`]). `Ok(None)` for nothing to claim, or a run with no book.
///
/// # Errors
/// Why no binary ran, as [`claim_memory`] says.
pub(crate) fn claim_test_resources(
    ctx: &Ctx,
    title: &str,
    bytes: u64,
    what: &str,
    cores: u32,
    admit: Admitter,
) -> Result<Option<ResourceClaim>, String> {
    let Some(book) = ctx.memory.as_ref().filter(|_| bytes > 0 || cores > 0) else {
        return Ok(None);
    };
    let why = format!("{} {what}", aterm_capacity::size(bytes));
    claim_bytes(ctx, title, bytes, &why, book.budget(), cores, admit).map(Some)
}

/// Claim `bytes` for `title` (`why` says what for) in this user's capacity ledger, admitted by
/// `admit` as [`claim_memory`] says, alone where they reach `budget`.
fn claim_bytes(
    ctx: &Ctx,
    title: &str,
    bytes: u64,
    why: &str,
    budget: u64,
    cores: u32,
    admit: Admitter,
) -> Result<ResourceClaim, String> {
    if let Some(why) = ended(ctx) {
        return Err(why);
    }
    let mode = if ctx.background {
        aterm_capacity::Admit::Preemptible
    } else {
        ctx.quiet.unwrap_or(aterm_capacity::Admit::Wait)
    };
    let ask = |alone: bool| {
        let need = aterm_capacity::Need::new(format!("verify {title}"))
            .mem(bytes)
            .cores(cores)
            .ends_when_stuck(STUCK_ENDS);
        let need = if bytes > 0 { need.covers() } else { need };
        if alone { need.alone() } else { need }
    };
    let beyond = bytes > 0 && bytes >= budget;
    let saying = claiming(why.to_owned());
    let mut admitted = admit(ask(beyond), mode);
    if !beyond && matches!(admitted, Err(aterm_capacity::Error::Never(_))) {
        admitted = admit(ask(true), mode);
    }
    drop(saying);
    let what = if beyond {
        format!(
            "claimed {why}: more than the {} that can ever be available here, so it asks to run \
             alone",
            aterm_capacity::size(budget)
        )
    } else if cores > 0 && bytes > 0 {
        format!("claimed {why} and up to {cores} cores")
    } else if cores > 0 {
        format!("claimed up to {cores} cores")
    } else {
        format!("claimed {why}")
    };
    let resource = if cores > 0 { "resource" } else { "memory" };
    match admitted {
        Ok(claim) => {
            let how = admitted_how(&claim, mode);
            Ok(ResourceClaim {
                claim: Arc::new(claim),
                said: format!(
                    "{}: {what}{how}",
                    if cores > 0 { "resources" } else { resource }
                ),
                units: None,
            })
        }
        Err(aterm_capacity::Error::Stopped { .. }) if ctx.halted().is_some() => {
            Err(ctx.halted().unwrap_or_default().to_owned())
        }
        Err(e @ aterm_capacity::Error::Stopped { .. }) => {
            eprintln!(
                "verify: {title}: its {resource} claim was {e}; the run ends, and what it did not \
                 decide COULD NOT RUN"
            );
            #[cfg(unix)]
            exec::group::stop_run();
            Err(format!(
                "its {resource} claim was {e}; it started nothing, and the run ends"
            ))
        }
        Err(e @ aterm_capacity::Error::Stuck { .. }) => {
            eprintln!("verify: {title}: its {resource} claim {e}; {title} decided nothing");
            Err(format!("its {resource} claim {e}; it started nothing"))
        }
        Err(e) => Err(format!(
            "its {resource} claim could not be admitted ({e}); it started nothing"
        )),
    }
}

fn before_stage_claim(ctx: &Ctx) -> Result<(), String> {
    #[cfg(unix)]
    if ctx.background && watch::running_should_stop(&ctx.root)? {
        exec::group::stop_run();
        return Err("a Release stopped the background sample".into());
    }
    Ok(())
}

/// One stage, run by its callback once its memory is claimed ([`claim_memory`]), on the run's
/// quiet machine when it is timed ([`quietly`]), with the machine read as it starts and as it
/// ends and its memory metered ([`memory::Meter`]): the report, when it started (after any
/// admission wait) and what the machine did meanwhile, with `held`, what the run's memory budget
/// held it back for ([`held_note`]), and what its claim did. A stage on the quiet machine
/// ([`takes_quiet`]) claims its memory only once it holds it: a run holds that across all of its
/// timed stages, so a timed stage of another run that held memory while it waited for it could
/// keep this run's next timed stage waiting for that memory, and neither would ever start. That
/// claim waits for the held claims alone, never behind a waiter (QUIET-FIRST, `aterm_capacity`),
/// so the quiet machine is not held through a turn in the queue. A stage holds its memory
/// share until it ends; bounded and pool workers retain their own shares until their work
/// ends. It waits for nothing while it holds one. A stage the run's end keeps
/// from starting ([`stopping`]), or whose memory or quiet claim an interrupt ended or gave up
/// behind holders that made no progress ([`STUCK_ENDS`]), is COULD NOT RUN.
fn staged(
    ctx: &Ctx,
    quiet: &Mutex<Quiet>,
    spec: &plan::StageSpec,
    held: Option<String>,
    seams: Seams,
    early: &EarlyClaims,
    run_stage: impl Fn() -> Report,
) -> (Report, Instant, String, Option<memory::ObservedCpu>) {
    let could_not_run = |why: String, held: Option<String>| {
        let mut report = Report::new(spec.title.clone());
        report.cannot_run(format!("{} — {why}", spec.title));
        (report, Instant::now(), held.unwrap_or_default(), None)
    };
    // Every child can contribute CPU observations; only metered stages keep memory records. The
    // test compile's meter attributes each compiler to its unit to date compile-line spans. Its
    // wrapper keeps maxrss pending until a passing stage promotes it with footprint; branded
    // Tippy admits and records only its enforceable whole-stage envelope.
    let meter = Some({
        if spec.id == plan::StageId::TestCompile {
            memory::Meter::attributing(ctx.unit_reader, memory::UNIT_FLOOR)
        } else {
            memory::Meter::new()
        }
    });
    let run = || {
        before_stage_claim(ctx)?;
        let claim = match early.take(spec.id) {
            Some(claim) => claim,
            None => claim_memory(ctx, spec, seams.admit_memory)?,
        };
        let _reservation =
            holding_resource_claim(claim.as_ref().map(|held| Arc::clone(&held.claim)));
        if let Some(why) = ended(ctx) {
            return Err(why);
        }
        let _metering = memory::metering(meter.clone());
        let _covering = memory::covering(claim.as_ref().and_then(ResourceClaim::cover));
        let jobs = compile_width::jobs_for(ctx, spec.id);
        let units = claim.as_ref().and_then(|held| held.units.as_ref());
        let (report, begun, machine) =
            compile_width::run_stage(spec.id, ctx.pool.as_deref(), jobs, units, &stopping, || {
                let begun = Instant::now();
                let from = aterm_capacity::Live.read(None);
                let report = run_stage();
                let to = aterm_capacity::Live.read(None);
                (report, begun, aterm_capacity::between(&from, &to))
            })
            .map_err(|e| format!("CPU tokens could not be acquired: {e}"))?;
        Ok((report, begun, machine, claim, jobs))
    };
    let ran = run_timed(ctx, quiet, spec.id, &spec.title, run);
    let ((report, begun, mut machine, claim, jobs), admitted) = match ran {
        Ok(ran) => ran,
        Err(why) => return could_not_run(why, held),
    };
    let claimed = claim.as_ref().map(|c| c.said.clone());
    for said in held.into_iter().chain(claimed).chain(admitted) {
        machine.push_str("; ");
        machine.push_str(&said);
    }
    let units = claim.as_ref().and_then(|claim| claim.units.as_ref());
    if let Some(said) = measured(ctx, spec, &report, meter.as_deref(), jobs, units) {
        machine.push_str("; ");
        machine.push_str(&said);
    }
    if let Some(units) = units
        && let Err(error) = compile_width::discard_pending(units)
    {
        machine.push_str("; compiler pending peaks not removed: ");
        machine.push_str(&error.to_string());
    }
    if let Some(part) = &spec.part
        && let Some(said) = ctx
            .shape_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .said
            .remove(&part.index)
    {
        machine.push_str("; ");
        machine.push_str(&said);
    }
    drop(claim);
    let cpu = meter.as_ref().and_then(|meter| meter.observed_cpu());
    (report, begun, machine, cpu)
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
    let pulled_without_forge = checks::with_pulled(&checks::DEV, false, holds);
    let appkit = (!ctx.release && own.contains(&Check::Appkit))
        .then(|| {
            checks::appkit_pull(&checks::DEV, false, cfg!(target_os = "macos"), |p| {
                ctx.scope.includes_crate(p)
            })
        })
        .flatten();
    let mut pulled_set = pulled_without_forge;
    if appkit.is_some() {
        pulled_set = checks::with(&pulled_set, Check::Appkit);
    }
    let looks_like_forge_pull = !ctx.release
        && own.contains(&Check::Forge)
        && own
            .iter()
            .copied()
            .filter(|check| *check != Check::Forge)
            .eq(pulled_set.iter().copied());
    let forge = looks_like_forge_pull
        .then(|| checks::forge_pull(&checks::DEV, false, &ctx.root, |p| ctx.scope.touches(p)))
        .flatten();
    if forge.is_some() {
        pulled_set = checks::with(&pulled_set, Check::Forge);
    }
    let is = |set: &[Check]| own.len() == set.len() && set.iter().all(|c| own.contains(c));
    let mut what = if ctx.release {
        if is(&checks::RELEASE) {
            "release".to_string()
        } else if is(&checks::RELEASE_TREE) {
            "release --tree".to_string()
        } else if is(&checks::RELEASE_BUNDLE) {
            "release --bundle".to_string()
        } else {
            format!("release: {}", names.join(" "))
        }
    } else if own == checks::DEV {
        "dev".to_string()
    } else if (!pulled.is_empty() || forge.is_some() || appkit.is_some()) && own == pulled_set {
        let (mut added, mut by): (Vec<&str>, Vec<&str>) =
            pulled.iter().map(|(c, p)| (c.name(), *p)).unzip();
        if let Some(reason) = forge.as_deref() {
            added.push(Check::Forge.name());
            by.push(reason);
        }
        if let Some(reason) = appkit {
            added.push(Check::Appkit.name());
            by.push(reason);
        }
        format!(
            "dev + {} (the selection holds {})",
            added.join(" + "),
            by.join(", ")
        )
    } else {
        names.join(" ")
    };
    if ctx.cells_pulled {
        what.push_str(
            " + cells (each other OS's cell and one of this OS's that builds a selected crate)",
        );
    }
    format!("verify: {what} — {}", selection(ctx, cone))
}

/// What the run's checks read.
#[must_use]
pub fn selection(ctx: &Ctx, cone: Option<&str>) -> String {
    if !ctx.checks.iter().any(|c| c.narrows()) && ctx.scope.is_workspace() {
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

/// [`run`], its stages run, and its quiet machine and its stages' memory admitted, by `seams`.
fn run_with(
    ctx: &Ctx,
    out: &mut (dyn Write + Send),
    log: &mut (dyn Write + Send),
    show: &Show<'_>,
    seams: Seams,
) -> std::io::Result<i32> {
    ctx.record_event(&format!(
        "verify-event: {}",
        crate::event_log::event_value(
            crate::event_log::EventKind::MeasureDriver,
            aterm_json::json!({"sha256":report::running_driver()})
        )
    ));
    if let Err(why) = test_workers::admitted_share(&ctx.env)
        .and_then(|_| test_workers::share(&ctx.env, ctx.pool.as_deref()))
    {
        writeln!(out, "verify: {why}; no check was run")?;
        let _ = writeln!(log, "verify: {why}; no check was run");
        return Ok(exit::USAGE);
    }
    if let Err(why) = (seams.prepare_build_dir)(ctx) {
        writeln!(
            out,
            "verify: could not prepare the build directory: {why}; no check was run"
        )?;
        let _ = writeln!(
            log,
            "verify: could not prepare the build directory: {why}; no check was run"
        );
        return Ok(exit::COULD_NOT_RUN);
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
    // Qualify's build branch has not started a compiler yet: reserve lint now, while its known
    // claim still fits around the build's already-admitted slice envelope. `staged` consumes the
    // same claim later, after the source preflight, and drops it when lint ends.
    let early = match EarlyClaims::for_run(ctx, &stages, seams.admit_memory) {
        Ok(early) => early,
        Err(why) => {
            let line = format!(
                "verify: COULD NOT RUN — early tippy memory admission could not run: {why}; no check was run"
            );
            ctx.record_event(&report::run_event(&[], exit::COULD_NOT_RUN));
            let _ = writeln!(log, "\n{line}");
            writeln!(out, "{line}")?;
            out.flush()?;
            return Ok(exit::COULD_NOT_RUN);
        }
    };
    // Readiness is diagnostic, not a success dependency of any native stage. Keep the captured
    // stock and terminal unready-cell results owned by this invocation, not by reusable Ctx.
    let cell_stage = stages
        .iter()
        .find(|stage| stage.id == plan::StageId::ForeignCells);
    let mut preparation = if cell_stage.is_some() && ended(ctx).is_none() {
        seams.prepare_cells.and_then(|prepare| prepare(ctx))
    } else {
        None
    };
    let mut observed = true;
    if let (Some(spec), Some(prepared)) = (cell_stage, preparation.as_mut())
        && prepared.needs_resolution()
    {
        // A failed admission defers resolution to the ordinary cells stage; it cannot invent
        // a missing-tool result or prevent the other checks from running.
        observed = false;
        if before_stage_claim(ctx).is_ok()
            && let Ok(claim) = claim_memory(ctx, spec, seams.admit_memory)
            && ended(ctx).is_none()
        {
            let _reservation =
                holding_resource_claim(claim.as_ref().map(|held| Arc::clone(&held.claim)));
            let _covering = memory::covering(claim.as_ref().and_then(ResourceClaim::cover));
            prepared.resolve_unready(ctx);
            observed = true;
        }
    }
    if let Some(prepared) = &preparation
        && observed
    {
        for (cell, triple, why) in prepared.unavailable(ctx) {
            let event = ctx.record_event(&format!(
                "verify-event: {}",
                crate::event_log::event_value(
                    crate::event_log::EventKind::CellPrerequisiteUnavailable,
                    aterm_json::json!({
                          "complete":false,
                        "cell":cell, "triple":triple, "detail":why
                    })
                )
            ));
            out.write_all(event.as_bytes())?;
            writeln!(
                out,
                "verify: cell prerequisite {cell} ({triple}): {why} (preparation only; cells check not finished)"
            )?;
            out.flush()?;
            let _ = writeln!(
                log,
                "cell prerequisite {cell} ({triple}): {why} (preparation only; cells check not finished)"
            );
        }
    }
    let preparation = Mutex::new(preparation);
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
    let budget = ctx.memory.as_ref().map_or(u64::MAX, memory::Book::budget);

    sched::run_stages(
        &stages,
        budget,
        &ctx.halt,
        |spec, held| {
            let i = stages
                .iter()
                .position(|s| std::ptr::eq(s, spec))
                .unwrap_or_default();
            let owner = owners[i].and_then(|c| ctx.checks.iter().position(|k| *k == c));
            let held = held_note(&stages, i, held, budget);
            let (report, begun_at, machine, cpu) =
                staged(ctx, &quiet, spec, held, seams, &early, || {
                    let event = ctx.record_event(&report::stage_started(i, &spec.title));
                    if let Ok(mut o) = out.lock() {
                        let _ = o.write_all(event.as_bytes()).and_then(|()| o.flush());
                    }
                    let prepared = if spec.id == plan::StageId::ForeignCells {
                        preparation
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                    } else {
                        None
                    };
                    if let Some(prepared) = prepared {
                        stages::run_stage_prepared(ctx, spec, Some(prepared))
                    } else {
                        (seams.stage)(ctx, spec)
                    }
                });
            let waiting = begun_at.saturating_duration_since(t0);
            let execution = begun_at.elapsed();
            // The ordered log and aggregate check result can wait for other stages;
            // the completed stage's decisions must already be visible to the caller.
            let event = ctx.record_event(&report::stage_event(
                i, owners[i], &report, waiting, execution, cpu,
            ));
            if let Ok(mut o) = out.lock() {
                let _ = o.write_all(event.as_bytes()).and_then(|()| o.flush());
            }
            if let Some(k) = owner
                && let Ok(mut p) = progress[k].lock()
            {
                p.started = Some(p.started.map_or(begun_at, |s| s.min(begun_at)));
            }
            if let Ok(mut c) = clocks.lock() {
                c[i] = Some((waiting, execution, machine));
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
                    let scope = if check == Check::Lint {
                        ctx.scope.lint_label()
                    } else if check.reads(&ctx.scope) {
                        ctx.scope.label()
                    } else {
                        String::new()
                    };
                    let result = report::judge(
                        check,
                        &mine,
                        seconds,
                        &scope,
                        ctx.rerun(check),
                        ctx.halted(),
                    );
                    // The check's record (A19) is read from this one line, written whole before
                    // its rendered line, so the two are never split by another output writer.
                    // Persist first: no output mutex is held during external file contention.
                    // Concurrent sinks may order different checks differently, but keep identical records.
                    let event = report::check_event(&result, &mine);
                    let text = report::render(&result, width);
                    let event = ctx.record_event(&event);
                    if let Ok(mut o) = out.lock() {
                        let _ = o
                            .write_all(event.as_bytes())
                            .and_then(|()| o.write_all(text.as_bytes()))
                            .and_then(|()| o.flush());
                    }
                    if let Ok(mut r) = results.lock() {
                        r.push(result);
                    }
                }
            }
            // After its own event and line, so a reader meets the red before what it stopped.
            if let Some(check) = owners[i] {
                red_now(ctx, check, &report);
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
    #[cfg(unix)]
    for evidence in aterm_uds::signal_origin::records() {
        let _ = writeln!(log, "{evidence}");
    }
    ctx.record_event(&report::run_event(&results, code));
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
    if ctx.compile_only {
        s.push_str(
            "test mode: compilation only; no test binaries execute and no release is qualified\n",
        );
    }
    if let Some(d) = &ctx.tools.demoted {
        s.push_str(&format!("toolchain note: {}\n", d.sentence()));
    }
    if let Some(note) = ctx.tools.borrowed_note() {
        s.push_str(&format!("toolchain note: {note}\n"));
    }
    if let Some(lease) = &ctx.toolchain_lease {
        s.push_str(&format!("toolchain lease: {lease}\n"));
    }
    if let Some(line) = lock_line(&ctx.tools) {
        s.push_str(&format!("toolchain lock: {line}\n"));
    }
    for note in &ctx.lock_notes {
        s.push_str(&format!("toolchain note: {note}\n"));
    }
    // Only the managed shared cache carries verification artifacts ([`Ctx::cached_lanes_dir`]).
    let shared = ctx.cached_lanes_dir.is_some();
    match (&ctx.main_build_dir, shared) {
        (Some(value), true) => {
            s.push_str(&format!(
                "build cache: shared intermediates ({value}), target {}\n",
                ctx.target_dir.display()
            ));
        }
        (Some(value), false) => s.push_str(&format!(
            "build cache: CARGO_TARGET_DIR={}, local intermediates ({value})\n",
            ctx.target_dir.display()
        )),
        (None, _) => s.push_str(&format!(
            "build cache: CARGO_TARGET_DIR={} (no managed intermediates directory)\n",
            ctx.target_dir.display()
        )),
    }
    if let Some(dir) = &ctx.cached_lanes_dir {
        s.push_str(&format!("verification cache: {}\n", dir.display()));
    }
    s
}

/// The build the tree's `trust.lock` names, as the log states it; `None` with no lock, or one that
/// does not read ([`lock_notes`] says so when something took its place and the run went on).
#[must_use]
pub fn lock_line(tools: &Toolchain) -> Option<String> {
    let found = tools.lock.as_ref()?;
    let lock = found.lock.as_ref().ok()?;
    Some(format!(
        "{} names Trust build {} (tree root {}, compiler {})",
        found.path.display(),
        lock.build,
        lock.tree_root.get(..12).unwrap_or(&lock.tree_root),
        lock.compiler_commit
            .get(..12)
            .unwrap_or(&lock.compiler_commit)
    ))
}

/// THE COMPILER EVERY COMPILE CHILD GETS, HELD TO THE LOCK (A16): children run the locked build's
/// `targo` by its path in `root`, with its `bin` first on `PATH`, and compile with the `trustc`
/// beside it, whose commit discovery already held to the lock's, unless something names another
/// compiler (`RUSTC`, `CARGO_BUILD_RUSTC`, a `build.rustc` in cargo's config from `root` up or in
/// `CARGO_HOME`: `aterm_toolchain::rustc_movers`). With a lock in force (no override said) the first
/// of those, resolved as cargo resolves it in the child, must answer the lock's `compiler_commit`.
/// `None` when it does, when none is set, or with no lock.
#[must_use]
pub fn lock_compiler_refusal(env: &EnvSnapshot, tools: &Toolchain, root: &Path) -> Option<String> {
    let found = tools
        .lock
        .as_ref()
        .filter(|_| tools.lock_override.is_none())?;
    let lock = found.lock.as_ref().ok()?;
    let cargo_home = env
        .cargo_home
        .clone()
        .unwrap_or_else(|| env.home.join(".cargo"));
    let var = |key: &str| {
        env.rustc_vars
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
    };
    let movers = aterm_toolchain::rustc_movers::rustc_movers(Some(root), &var, Some(&cargo_home));
    aterm_toolchain::rustc_movers::lock_refusal(
        &movers,
        &tools.path_with_stage2_first(&env.path),
        &found.path,
        lock,
    )
}

/// THE TOOLCHAIN A RUN AT `root` COMPILES WITH, or why it compiles with none: [`run_toolchain`],
/// refused when it has no runnable `targo` or when another compiler takes its place in the
/// children ([`lock_compiler_refusal`]).
///
/// # Errors
/// The refusal, for a COULD NOT RUN.
pub fn preflight(env: &EnvSnapshot, root: &Path) -> Result<Toolchain, String> {
    let tools = run_toolchain(env, root);
    if !tools.have_targo() {
        return Err(tools.missing_targo_label());
    }
    if let Some(why) = lock_compiler_refusal(env, &tools, root) {
        return Err(why);
    }
    Ok(tools)
}

/// WHAT A RUN SAYS OF ITS TREE'S `trust.lock` (A14), on stderr and in the log: what took the
/// lock's place (`TRUST_STAGE2_BIN`, a dev-linked checkout), a lock that does not read, and a
/// `targo` shim on this Mac that runs another build than this run compiles with, so plain builds
/// and verify compile alternate units (`atpkg::trust_lock::shim_mismatch`). Empty with no lock.
#[must_use]
pub fn lock_notes(env: &EnvSnapshot, tools: &Toolchain) -> Vec<String> {
    let Some(found) = tools.lock.as_ref() else {
        return Vec::new();
    };
    let mut notes: Vec<String> = tools.lock_override.iter().cloned().collect();
    let lock = match found.lock.as_ref() {
        Ok(lock) => lock,
        Err(why) => {
            notes.push(format!("{} does not read: {why}", found.path.display()));
            return notes;
        }
    };
    let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
    let shim = atpkg::trust_lock::shim_answer(&prefix);
    notes.extend(atpkg::trust_lock::shim_mismatch(
        &shim,
        found,
        lock,
        &tools.stage2_dir,
    ));
    notes
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

/// The checks a parsed invocation runs after selection-sensitive pulls. This is the one planner
/// used by [`run_cli`]: tests exercise it directly so a check cannot be implemented correctly but
/// left disconnected from the command that lands changes.
fn planned_checks_for_host(
    parsed: &cli::Args,
    scope: &Scope,
    root: &Path,
    macos: bool,
) -> (Vec<Check>, bool) {
    let mut wanted = parsed.wanted(|package| scope.includes_crate(package));
    if checks::forge_pull(
        &parsed.checks,
        parsed.release || parsed.as_release,
        root,
        |package| scope.touches(package),
    )
    .is_some()
    {
        wanted = checks::with(&wanted, Check::Forge);
    }
    if checks::appkit_pull(
        &parsed.checks,
        parsed.release || parsed.as_release,
        macos,
        |package| scope.includes_crate(package),
    )
    .is_some()
    {
        wanted = checks::with(&wanted, Check::Appkit);
    }
    let cells_pulled = checks::pulls_cells(&wanted, parsed.release, !scope.selects_nothing());
    if cells_pulled {
        wanted = checks::with(&wanted, Check::Cells);
    }
    (wanted, cells_pulled)
}

fn planned_checks(parsed: &cli::Args, scope: &Scope, root: &Path) -> (Vec<Check>, bool) {
    planned_checks_for_host(parsed, scope, root, cfg!(target_os = "macos"))
}

/// The whole CLI, in-process.
#[must_use]
pub fn run_cli(args: &[String], cwd: &Path) -> i32 {
    if args.first().is_some_and(|arg| arg == "release-input") {
        return release_input::run_cli(&args[1..], cwd);
    }
    if args.first().is_some_and(|arg| arg == "status") {
        return status::run(&args[1..], cwd);
    }
    #[cfg(unix)]
    if args.first().is_some_and(|a| a == "watch") {
        return watch::run(&args[1..], cwd);
    }
    #[cfg(unix)]
    if args.first().is_some_and(|a| a == "land") {
        return land::run(&args[1..], cwd);
    }
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
    let passed_binaries = match parsed.passed_binaries.as_deref().map(|path| {
        aterm_uds::atomic_file::read_regular_text(&cwd.join(path), 16 * 1024 * 1024)
            .map_err(|e| e.to_string())
            .and_then(|text| testrun::read_passed(&text))
            .map_err(|why| format!("verify: --passed-binaries {}: {why}", path.display()))
    }) {
        Some(Err(why)) => return stop(exit::USAGE, why),
        Some(Ok(passed)) => Some(passed),
        None => None,
    };
    let passed_lint_packages = match parsed.passed_lint_packages.as_deref().map(|path| {
        aterm_uds::atomic_file::read_regular_text(&cwd.join(path), 16 * 1024 * 1024)
            .map_err(|error| error.to_string())
            .and_then(|text| lint_content::read_passed(&text))
            .map_err(|why| format!("verify: --passed-lint-packages {}: {why}", path.display()))
    }) {
        Some(Err(why)) => return stop(exit::USAGE, why),
        Some(Ok(passed)) => Some(passed),
        None => None,
    };
    let passed_appkit_groups = match parsed.passed_appkit_groups.as_deref().map(|path| {
        aterm_uds::atomic_file::read_regular_text(&cwd.join(path), 64 * 1024)
            .map_err(|error| error.to_string())
            .and_then(|text| appkit_content::read_passed(&text))
            .map_err(|why| format!("verify: --passed-appkit-groups {}: {why}", path.display()))
    }) {
        Some(Err(why)) => return stop(exit::USAGE, why),
        Some(Ok(passed)) => Some(passed),
        None => None,
    };
    let events = match parsed
        .events
        .as_deref()
        .map(|path| event_log::EventLog::open(&cwd.join(path)).map_err(|e| (path, e)))
        .transpose()
    {
        Ok(events) => events,
        Err((path, e)) => return could_not_run(format!("--events {}: {e}", path.display())),
    };
    if let Some(note) = cli::ignored_selection(&parsed) {
        eprintln!("verify: {note}");
    }
    if git_provider::pin().is_none() {
        return could_not_run("no absolute executable Git on PATH".into());
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
    let pool = match env
        .release_pool
        .as_deref()
        .map(aterm_capacity::Jobserver::adopt)
    {
        None => None,
        Some(Ok(pool)) => Some(pool),
        Some(Err(why)) => {
            return could_not_run(format!(
                "the release's pool ({}) cannot be adopted: {why}; no check was run",
                aterm_capacity::POOL_ENV
            ));
        }
    };
    let tools = match preflight(&env, &root) {
        Ok(tools) => tools,
        Err(why) => return could_not_run(why),
    };
    if let Some(d) = &tools.demoted {
        eprintln!("verify: toolchain: {}", d.sentence());
    }
    // A conflicted or plain `git merge` leaves vendor/astream checked out at the branch's old
    // pin, and staging that records it: forge's own OB-1 pin judgments refuse both before
    // anything builds the older astream, in every run, not only those that pull `forge`.
    if let Err(why) = aterm_forge::astream_pin(&root) {
        return stop(
            exit::FAILED,
            format!("verify: FAIL — [OB-1] {why}; no check was run"),
        );
    }
    if parsed.builds_packages_here() {
        let path_env = tools.path_with_stage2_first(&env.path);
        let foreign = changed::foreign_specs(&root, &tools, &path_env, &parsed.packages);
        if !foreign.is_empty() {
            return stop(exit::USAGE, changed::foreign_refusal(&foreign));
        }
    }
    let lock_said = lock_notes(&env, &tools);
    for note in &lock_said {
        eprintln!("verify: toolchain: {note}");
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
        runs::end_dead_runs(
            dir,
            &boot,
            me,
            &runs::LiveKernel,
            &mut |line| {
                eprintln!("verify: {line}");
            },
            &runs::ToInstance::from_env(),
        );
    }
    #[cfg(target_os = "macos")]
    if let Some(line) = runs::program_check_line(&runs::LiveKernel, || {
        let me = i32::try_from(std::process::id()).unwrap_or(0);
        let (app, listed) =
            atpkg::devtools::listed_for_responsible(me, window_list::hosting_instance);
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
    let (wanted, cells_pulled) = planned_checks(&parsed, &scope, &root);
    let mut ctx = Ctx::new_with_tools(root, scope, env, scratch.clone(), tools)
        .with_checks(&wanted)
        .with_release(parsed.release)
        .with_checkers_in_tests(parsed.release || parsed.as_release)
        .with_fix(parsed.fix)
        .with_cells(parsed.cells.clone())
        .with_cells_pulled(cells_pulled)
        .with_no_windows(parsed.no_windows)
        .with_halt(halt_of(&parsed))
        .with_cursor_pacing(parsed.cursor_pacing)
        .with_compile_only(parsed.compile_only)
        .with_measure_artifact(measure_artifact)
        .with_child_ceiling(parsed.timeout.unwrap_or(Some(exec::DEFAULT_CHILD_CEILING)))
        .with_pool(pool)
        .with_test_threads(parsed.test_threads)
        .with_test_cores(parsed.test_cores)
        .with_passed_binaries(passed_binaries)
        .with_passed_lint_packages(passed_lint_packages)
        .with_passed_appkit_groups(passed_appkit_groups)
        .with_carry_context(parsed.carry_context.clone())
        .with_input_drift(parsed.input_drift)
        .with_events(events)
        .with_quiet(Some(if parsed.background {
            aterm_capacity::Admit::Preemptible
        } else {
            parsed.admit
        }))
        .with_memory(memory::Book::live())
        .with_test_history(test_history::planning_path(
            parsed.test_history.clone(),
            aterm_types::dirs::account_state_dir(),
        ))
        .with_toolchain_lease(lease_said)
        .with_lock_notes(lock_said)
        .with_quiet_tmpdir(&std::env::temp_dir())
        .with_head_at_start(head_at_start);
    ctx.background = parsed.background;
    #[cfg(target_os = "macos")]
    {
        ctx.screen_locked = smoke::screen_locked;
    }
    ctx.test_seed = parsed.test_seed;
    if let Err(error) = prepare_main_build_dir(&ctx) {
        end_run(&ctx, &scratch);
        return could_not_run(format!("cannot prepare the captured build plan: {error}"));
    }
    let mut ctx = match runs_dir
        .as_deref()
        .and_then(|dir| runs::Runs::open(&ctx, dir, exec::run))
        .map(|record| record.telling(Box::new(runs::ToInstance::from_env())))
    {
        Some(record) => ctx.with_runs(record),
        None => ctx,
    };
    #[cfg(unix)]
    for line in plan::retire_old_lanes(&ctx) {
        eprintln!("verify: {line}");
    }
    #[cfg(unix)]
    let disk = match disk::admit(&ctx, run_admit_mode(parsed.background, parsed.admit)) {
        Ok(held) => held,
        Err(why) => {
            if let Some(record) = &ctx.runs {
                record.finish(&runs::LiveKernel);
            }
            std::fs::remove_dir_all(&scratch).ok();
            let code = could_not_run(why);
            exec::group::end_interrupted();
            return code;
        }
    };
    let mut log = open_run_log(&mut ctx, parsed.log.as_deref(), cwd);
    #[cfg(unix)]
    if let Some(log) = &log
        && let Err(why) = log.observe_signals(ctx.events.clone())
    {
        eprintln!("signal log unavailable: {why}");
    }
    ctx.hang_samples = log.as_ref().map(|log| run_log::sample_dir(&log.path));
    let log_shown = log.as_ref().map(|log| log.path.display().to_string());
    let mut sink = std::io::sink();
    let log_writer: &mut (dyn Write + Send) = match log.as_mut() {
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
    {
        if let Some(line) = disk::keep_used(&ctx, code == exit::PASS) {
            let _ = log.as_mut().map(|f| writeln!(f, "{line}"));
        }
        let_disk_go(disk, code == exit::PASS, log.as_mut(), ctx.events.as_ref());
    }
    end_run(&ctx, &scratch);
    if parsed.background && stopping() && code != exit::FAILED {
        75
    } else {
        code
    }
}

/// Admit paired diagnostics before stages begin, binding only captured context facts.
fn open_run_log(ctx: &mut Ctx, chosen: Option<&Path>, cwd: &Path) -> Option<run_log::Log> {
    let event_context = event_log::Context::captured(ctx);
    ctx.events
        .get_or_insert_with(event_log::EventLog::unrecorded)
        .bind(event_context);
    let invocation = ctx
        .events
        .as_ref()
        .and_then(event_log::EventLog::id)
        .map(str::to_owned);
    let mut log = run_log::open(
        chosen,
        cwd,
        &run_log::default_dir(&ctx.state_dir()),
        invocation.as_deref(),
    );
    if let Some(retained) = log.as_mut().and_then(run_log::Log::take_events) {
        if let Some(events) = &mut ctx.events {
            events.retain(retained);
        }
        if let Some(log) = &log {
            eprintln!(
                "verify: events: {}",
                run_log::events_path(&log.path).display()
            );
        }
    }
    event_log::emit(
        ctx.events.as_ref(),
        &format!(
            "verify-event: {}",
            event_log::event_value(
                event_log::EventKind::InvocationStarted,
                aterm_json::json!({})
            )
        ),
    );
    log
}

/// Let the run's disk claim `held` go, saying in `log` what it claimed and what the run grew:
/// what it took kept for the next run of its scope on its directories and, where it `passed`, in
/// this machine's record for every checkout (`atpkg::admit_disk::Admitted::finish_whole`; a run
/// that failed may have stopped short of what its scope writes, so it is kept for its own
/// directories alone, `atpkg::admit_disk::Admitted::finish_here`), unless an interrupt or its
/// first proven red ended the run ([`stopping`]), which stopped short of its work and so wrote
/// less than a run of its scope writes (`atpkg::admit_disk::Admitted::abandon`). The line says
/// which records took it ([`kept_line`]). A run the work it is part of covered
/// ([`disk::Disk::Covered`]) walked and keeps nothing, and its line says whose claim covered it.
/// Then, whatever the run's disk, it says on stderr how it was held and how many walks of its
/// directories it made ([`disk::event`]).
#[cfg(unix)]
fn let_disk_go(
    held: disk::Disk,
    passed: bool,
    log: Option<&mut impl Write>,
    events: Option<&event_log::EventLog>,
) {
    let mode = held.mode();
    let held = match held {
        disk::Disk::Held(held) => *held,
        disk::Disk::Covered(by) => {
            let _ = log.map(|f| writeln!(f, "{}", disk::covered_line(&by)));
            event_log::emit(events, &disk::event(mode, aterm_capacity::walks()));
            return;
        }
        disk::Disk::Unclaimed => {
            event_log::emit(events, &disk::event(mode, aterm_capacity::walks()));
            return;
        }
    };
    let claimed = held.estimate;
    let (took, kept) = if stopping() {
        let why = if exec::halted().is_some() {
            "stopped at its first red"
        } else {
            "was interrupted"
        };
        (
            held.abandon(),
            format!("; the run {why}, so that is not kept for its scope"),
        )
    } else {
        let done = if passed {
            held.finish_whole()
        } else {
            held.finish_here()
        };
        let kept = kept_line(passed, &done);
        (done.run, kept)
    };
    let line = format!(
        "disk: claimed {claimed}; this run grew its directories by {} (they held {} at most){kept}",
        atpkg::admit_disk::size(took.grew),
        atpkg::admit_disk::size(took.peak)
    );
    let _ = log.map(|f| writeln!(f, "{line}"));
    event_log::emit(events, &disk::event(mode, aterm_capacity::walks()));
}

/// What a finished run's disk line says it kept (`let_disk_go`): which of the record beside its
/// directories and the machine's record for its other checkouts took it, and why the second did
/// not where it did not.
#[cfg(unix)]
fn kept_line(passed: bool, done: &atpkg::admit_disk::Finished) -> String {
    let here = match &done.here {
        Ok(()) => "; kept for its scope here".to_owned(),
        Err(e) => format!("; not kept for its scope here: its record could not be written ({e})"),
    };
    let machine = match &done.machine {
        _ if !passed => "; it did not pass, so not for this machine's other checkouts".to_owned(),
        Some(Ok(())) => "; kept for this machine's other checkouts".to_owned(),
        Some(Err(e)) => {
            format!(
                "; not kept for this machine's other checkouts: its record could not be written ({e})"
            )
        }
        None => "; not kept for this machine's other checkouts: this run keeps no record of them \
                 (no account state directory, or a build directory verify does not place)"
            .to_owned(),
    };
    format!("{here}{machine}")
}

/// The end of a run that has reported ([`run_with`]) and let its disk claim go (`let_disk_go`):
/// its run record finished and its `scratch` removed. Then an interrupt the handler took ends it:
/// it dies of that signal, as it did before it reported (`exec::group::end_interrupted`).
fn end_run(ctx: &Ctx, scratch: &Path) {
    if let Some(record) = &ctx.runs {
        record.finish(&runs::LiveKernel);
    }
    std::fs::remove_dir_all(scratch).ok();
    #[cfg(unix)]
    exec::group::end_interrupted();
}

/// A private scratch directory named for `tag`, made atomically by [`aterm_tempfile`]. Unix
/// uses the short `/tmp` root (its `.ubd` folder on macOS), keeping smoke socket paths below
/// macOS's 104-byte `sockaddr_un` limit. Elsewhere it uses the system temporary directory.
/// The caller removes it; the `.noindex` suffix keeps the tree out of Spotlight too.
///
/// # Errors
/// Fails when the temporary directory refuses creation or `tag` contains a path separator.
pub fn mktemp_dir(tag: &str) -> std::io::Result<PathBuf> {
    let builder = aterm_tempfile::Builder::new()
        .prefix(&format!("{tag}."))
        .suffix(".noindex");
    let directory = if cfg!(unix) {
        builder.tempdir_in("/tmp")
    } else {
        builder.tempdir()
    }?;
    Ok(directory.keep())
}

/// Put an executable `#!/bin/sh` stand-in running `body` at `path`, replacing whatever is there.
///
/// `path` is a symlink to its text's one read-only file in this user's stand-in cache: one file
/// per text to a process, the one a later process finds whole, though first writers that raced
/// may each run a slot of their own ([`aterm_tempfile::shared_stand_in`] says why that file has
/// one name and no fixture holds one); where that cache cannot be used, the stand-in is written
/// at `path` itself, mode 0755, and runs no file of the cache. Off Apple a child process writes
/// it (`aterm_tempfile::written_by_a_child`), so no fork of this process holds it open for
/// writing when it runs (Linux refuses that exec, `ETXTBSY`).
///
/// # Panics
/// When `path`'s directory cannot be made or the stand-in cannot be written.
#[cfg(unix)]
#[doc(hidden)]
pub fn standin(path: &Path, body: &str) {
    let text = format!("#!/bin/sh\n{body}\n");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    let _ = std::fs::remove_file(path);
    if aterm_tempfile::shared_stand_in(&text)
        .is_some_and(|file| std::os::unix::fs::symlink(file, path).is_ok())
    {
        return;
    }
    #[cfg(not(target_vendor = "apple"))]
    aterm_tempfile::written_by_a_child(path, text.as_bytes(), 0o755)
        .unwrap_or_else(|e| panic!("write the stand-in {}: {e}", path.display()));
    #[cfg(target_vendor = "apple")]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        use std::os::unix::fs::PermissionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .expect("open");
        file.write_all(text.as_bytes()).expect("write");
        file.set_permissions(std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
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
    write_fresh(&tmp, format!("{body}\n")).expect("write the stand-in's body");
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
pub use aterm_uds::host::host_user_cache;

#[cfg(all(test, unix))]
mod quiet_interrupt_tests;

#[cfg(all(test, unix))]
mod forge_pull_e2e_tests;

#[cfg(test)]
mod timing_test_machine;

#[cfg(test)]
mod quiet_renewal_tests;

#[cfg(all(test, unix))]
mod interrupt_report_tests;

#[cfg(all(test, unix))]
mod halt_report_tests;

#[cfg(test)]
mod libtest_fixture;

#[cfg(all(test, unix))]
mod live_halt_tests;

#[cfg(all(test, unix))]
mod memory_claim_tests;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
