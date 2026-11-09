// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-verify` — every aterm check as one CLI: `targo --unverified verify [CHECK|SET ...]`.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: the verify CLI spawns its checks"
)]

// These fixtures judge write ordering and recovery. The durable crate judges the hardware
// flush itself; keep every requested flush here without draining the SSD for each fixture.
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

#[cfg(unix)]
pub mod browser;
pub mod cells;
pub mod changed;
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
mod harness_debt;
pub mod ladder;
pub mod libtest;
pub mod libtest_json;
pub mod lint_content;
pub mod memory;
pub mod perf;
pub mod plan;
#[cfg(unix)]
pub mod provision;
pub mod release_input;
pub mod report;
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
    /// The variables that choose cargo's layout, raw and only when set ([`CARGO_LAYOUT_VARS`]).
    pub cargo_layout: Vec<(&'static str, OsString)>,
    /// `RUST_TEST_THREADS`, raw.
    pub rust_test_threads: Option<OsString>,
    /// `CARGO_HOME`.
    pub cargo_home: Option<PathBuf>,
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
            cargo_target_dir: var_path("CARGO_TARGET_DIR")
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| std::path::absolute(&p).unwrap_or(p)),
            cargo_home: var_path("CARGO_HOME").filter(|p| p.is_absolute()),
            cargo_layout: CARGO_LAYOUT_VARS
                .iter()
                .filter_map(|k| Some((*k, std::env::var_os(k)?)))
                .collect(),
            rust_test_threads: std::env::var_os("RUST_TEST_THREADS"),
            native_packages,
            publication_engine: std::env::var_os("PUBLICATION_ENGINE").filter(|v| !v.is_empty()),
            disk_cover: std::env::var(aterm_capacity::DISK_COVER_ENV)
                .ok()
                .filter(|m| !m.trim().is_empty()),
        }
    }
}

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

/// The run's target directory, physical ([`physical`]): the caller's `CARGO_TARGET_DIR`, else
/// `<root>/target`. Every child but a side lane's (handed its lane directory, physical too) gets it
/// as `CARGO_TARGET_DIR` ([`Ctx::child_env_add`]), so cargo spells each `CARGO_BIN_EXE_*` and
/// `CARGO_TARGET_TMPDIR` it compiles into a test through it and never through a link: a release
/// clone reaches its target through `target -> target.noindex`, and a test that resolved one
/// spelling and compared it with the other failed there. Cargo does not fingerprint either value,
/// so a test binary compiled before keeps its spelling until it recompiles. A missing directory is
/// not created: the spelling of its existing ancestor decides.
#[must_use]
pub fn physical_target(env: &EnvSnapshot, root: &Path) -> PathBuf {
    physical(
        &env.cargo_target_dir
            .clone()
            .unwrap_or_else(|| root.join("target")),
    )
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
    /// The caller's `CARGO_TARGET_DIR`, else `<root>/target`, physical ([`physical_target`]):
    /// the `CARGO_TARGET_DIR` of every child but a side lane's ([`Self::child_env_add`]).
    pub target_dir: PathBuf,
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
    /// Side-lane artifacts under the managed build cache, when the shim selected one.
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
    /// retains bounded protection through its stages. `Some(Now)` cannot qualify Timing and
    /// starts no timed callback. `None` is the caller's explicit untimed/outer-owned contract;
    /// it supplies no CPU observation or measurement qualification.
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
    /// The test binaries a release's resume carries ([`testrun::carry`], `--passed-binaries`).
    pub passed_binaries: Option<testrun::Passed>,
    /// Per-package lint successes a release resume may carry by complete content key.
    pub passed_lint_packages: Option<lint_content::Passed>,
    /// The release cutter's frozen verifier, seal and toolchain context, salted into each test
    /// binary and lint-package content key (`--carry-context`).
    pub carry_context: Option<String>,
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
    /// What the run's first proven red ends (A3): under `release` and `--fail-fast` the rest of
    /// the run, which then says NOT RUN with the halt's words; in a bare run nothing.
    pub halt: sched::Halt,
    /// What resolves a call's unit graph for the test compile's planner ([`shape::shape`]): its
    /// `--unit-graph` document, or why not; [`stages::unit_graph`] (resolved, never built) but
    /// in a test.
    pub unit_graph: fn(&Ctx, &shape::Piece) -> Result<String, String>,
    /// What the run's shaped test compile has built so far, and what the fresh check said
    /// ([`shape::fresh_check`]).
    pub shape_run: Mutex<shape::Run>,
    /// When the run began (its context was made), which a release run's record of what it used
    /// names ([`disk::keep_used`]): a unit of the main lane's build directory written since is one
    /// some compile of the run built.
    pub began: std::time::SystemTime,
    /// What names each compiler of a test compile or a lint for its unit, its span and, where it
    /// is heavy, its unit's record ([`memory::read_unit`] but in a test).
    pub unit_reader: memory::UnitReader,
    /// The release's pool this run adopted from [`EnvSnapshot::release_pool`] ([`run_cli`]):
    /// the test run's share, its binaries' charges held in its tokens
    /// ([`testrun::dispatch_pooled`]); `None` outside a release.
    pub pool: Option<aterm_capacity::Jobserver>,
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
        let target_dir = physical_target(&env, &root);
        let shim_build_dir = main_build_dir(&env, &root);
        // Physical as the target is: a side lane hands its children this as their target.
        let cached_lanes_dir = shim_build_dir
            .as_deref()
            .and_then(|value| disk::build_dir_path(value, &root))
            .map(|dir| physical(&dir.join("verify").join(aterm_tempfile::QUIET)));
        // The main lane names its build directory itself (`--config build.build-dir`), so the
        // target this pins never moves the shim's cache; a side lane sets its own.
        let mut child_env_add: Vec<(OsString, OsString)> = vec![(
            "CARGO_TARGET_DIR".into(),
            target_dir.clone().into_os_string(),
        )];
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
            carry_context: None,
            events: None,
            hang_samples: None,
            test_compile_failed: std::sync::atomic::AtomicBool::new(false),
            halt: sched::Halt::off(),
            unit_graph: stages::unit_graph,
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

    /// The CPU share the test binaries are charged from ([`test_workers::share`]): the release's
    /// pool's slots where a pool was adopted, the admitted share where a slot is held
    /// ([`test_workers::admitted_share`]), else the host's cores. A context inspected before
    /// [`run_with`] refuses malformed admission data (or a pool named and not adopted) still
    /// names no unbounded share: one core. `--test-cores` caps this request without granting
    /// capacity; the execution stage can receive less from admission.
    #[must_use]
    pub fn test_share(&self) -> u32 {
        let available = match test_workers::share(&self.env, self.pool.as_ref()) {
            Ok(test_workers::Share::Pool(slots)) => slots,
            Ok(test_workers::Share::Claim(share)) => share.unwrap_or_else(|| self.cores()),
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
        self.pool = pool;
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

    /// Carry the test binaries `passed` names into this release's test run, each whose
    /// executable still hashes so ([`Ctx::passed_binaries`]).
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

    /// Salt test-binary and lint-package evidence with the release's frozen verifier, seal and
    /// toolchain context.
    #[must_use]
    pub fn with_carry_context(mut self, context: Option<String>) -> Self {
        self.carry_context = context;
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

/// What the run's first proven red ends ([`Ctx::halt`]): a release, and a `--fail-fast` run, whose
/// red stops every child the run started ([`exec::group::halt`]); a bare run, nothing.
fn halt_of(release: bool, fail_fast: bool) -> sched::Halt {
    #[cfg(unix)]
    let end: fn(&str) = exec::group::halt;
    #[cfg(not(unix))]
    let end: fn(&str) = |_| {};
    if release {
        sched::Halt::at_first_red("the release", end)
    } else if fail_fast {
        sched::Halt::at_first_red("the run", end)
    } else {
        sched::Halt::off()
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
    let out = f();
    quiet
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .passed();
    Ok((out, Some(said)))
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
    // The book's unit rows are the test compile's: a lint's compilers hold other peaks.
    let unit_peaks = if spec.id == plan::StageId::TestCompile {
        meter.unit_peaks()
    } else {
        std::collections::BTreeMap::new()
    };
    match book.keep_units(&key.checkout, &unit_peaks) {
        Ok(kept) if !kept.is_empty() => {
            let heaviest: Vec<String> = kept
                .iter()
                .take(3)
                .map(|(n, p)| format!("{} {}", n.kind, aterm_capacity::size(*p)))
                .collect();
            let more = if kept.len() > 3 { ", …" } else { "" };
            said.push_str(&format!(
                "; its heaviest units kept: {}{more}",
                heaviest.join(", ")
            ));
        }
        Ok(_) => {}
        Err(e) => said.push_str(&format!("; its units' peaks not kept: {e}")),
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
}

impl ResourceClaim {
    /// The cover marker its stage's children are handed ([`memory::cover`]).
    pub(crate) fn cover(&self) -> Option<std::sync::Arc<str>> {
        (self.claim.record().need.mem > 0)
            .then(|| std::sync::Arc::from(self.claim.record().cover_marker()))
    }
}

/// The CPU width a repeatable tree stage asks for. A caller's smaller explicit build ceiling is
/// retained; otherwise the host's parallelism is the upper bound. Capacity may grant less.
fn background_cores(ctx: &Ctx) -> Option<u32> {
    ctx.background.then(|| {
        ctx.build_jobs_ceiling()
            .unwrap_or_else(|| ctx.cores())
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
    let need = book.need(spec, &memory::Key::of_stage(spec, ctx));
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
    match admitted {
        Ok(claim) => {
            let how = admitted_how(&claim, mode);
            Ok(ResourceClaim {
                claim: Arc::new(claim),
                said: format!(
                    "{}: {what}{how}",
                    if cores > 0 { "resources" } else { "memory" }
                ),
            })
        }
        Err(aterm_capacity::Error::Stopped { .. }) if ctx.halted().is_some() => {
            Err(ctx.halted().unwrap_or_default().to_owned())
        }
        Err(e @ aterm_capacity::Error::Stopped { .. }) => {
            eprintln!(
                "verify: {title}: its memory claim was {e}; the run ends, and what it did not \
                 decide COULD NOT RUN"
            );
            #[cfg(unix)]
            exec::group::stop_run();
            Err(format!(
                "its memory claim was {e}; it started nothing, and the run ends"
            ))
        }
        Err(e @ aterm_capacity::Error::Stuck { .. }) => {
            eprintln!("verify: {title}: its memory claim {e}; {title} decided nothing");
            Err(format!("its memory claim {e}; it started nothing"))
        }
        Err(e) => Err(format!(
            "its memory claim could not be admitted ({e}); it started nothing"
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
    on_started: impl Fn(),
    run_stage: impl Fn() -> Report,
) -> (Report, Instant, String, Option<memory::ObservedCpu>) {
    let could_not_run = |why: String, held: Option<String>| {
        let mut report = Report::new(spec.title.clone());
        report.cannot_run(format!("{} — {why}", spec.title));
        (report, Instant::now(), held.unwrap_or_default(), None)
    };
    // Every child can contribute CPU observations; only metered stages keep memory
    // records. The test compile's and the lint's meters attribute each compiler to its unit,
    // which dates their compile lines' spans; only the test compile's keeps unit peaks, and only
    // into the memory book ([`measured`]).
    let meter = Some({
        if matches!(spec.id, plan::StageId::TestCompile | plan::StageId::Tippy) {
            memory::Meter::attributing(ctx.unit_reader, memory::UNIT_FLOOR)
        } else {
            memory::Meter::new()
        }
    });
    let run = || {
        before_stage_claim(ctx)?;
        let claim = claim_memory(ctx, spec, seams.admit_memory)?;
        let _reservation =
            holding_resource_claim(claim.as_ref().map(|held| Arc::clone(&held.claim)));
        if let Some(why) = ended(ctx) {
            return Err(why);
        }
        let _metering = memory::metering(meter.clone());
        let _covering = memory::covering(claim.as_ref().and_then(ResourceClaim::cover));
        let jobs = compile_width::stage_jobs(
            spec.id,
            ctx.pool.as_ref().map(aterm_capacity::Jobserver::slots),
            ctx.build_jobs_ceiling(),
        );
        let (report, begun, machine) =
            compile_width::run(ctx.pool.as_ref(), jobs, &stopping, || {
                let begun = Instant::now();
                let from = aterm_capacity::Live.read(None);
                on_started();
                let report = run_stage();
                let to = aterm_capacity::Live.read(None);
                (report, begun, aterm_capacity::between(&from, &to))
            })
            .map_err(|e| format!("compiler tokens could not be acquired: {e}"))?;
        Ok((report, begun, machine, claim, jobs))
    };
    let ran = if takes_quiet(ctx, spec.id) {
        quietly(ctx, quiet, &spec.title, run).and_then(|(ran, admitted)| Ok((ran?, admitted)))
    } else {
        run().map(|ran| (ran, None))
    };
    let ((report, begun, mut machine, claim, jobs), admitted) = match ran {
        Ok(ran) => ran,
        Err(why) => return could_not_run(why, held),
    };
    let claimed = claim.as_ref().map(|c| c.said.clone());
    for said in held.into_iter().chain(claimed).chain(admitted) {
        machine.push_str("; ");
        machine.push_str(&said);
    }
    if let Some(said) = measured(ctx, spec, &report, meter.as_deref(), jobs) {
        machine.push_str("; ");
        machine.push_str(&said);
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
    let looks_like_forge_pull = !ctx.release
        && own.contains(&Check::Forge)
        && own
            .iter()
            .copied()
            .filter(|check| *check != Check::Forge)
            .eq(pulled_without_forge.iter().copied());
    let forge = looks_like_forge_pull
        .then(|| checks::forge_pull(&checks::DEV, false, &ctx.root, |p| ctx.scope.touches(p)))
        .flatten();
    let mut pulled_set = pulled_without_forge;
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
    } else if (!pulled.is_empty() || forge.is_some()) && own == pulled_set {
        let (mut added, mut by): (Vec<&str>, Vec<&str>) =
            pulled.iter().map(|(c, p)| (c.name(), *p)).unzip();
        if let Some(reason) = forge.as_deref() {
            added.push(Check::Forge.name());
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
        aterm_json::json!({"schema":1,"event":"measure-driver","sha256":report::running_driver()})
    ));
    if let Err(why) = test_workers::admitted_share(&ctx.env)
        .and_then(|_| test_workers::share(&ctx.env, ctx.pool.as_ref()))
    {
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
                aterm_json::json!({
                    "schema":1, "event":"cell-prerequisite-unavailable", "complete":false,
                    "cell":cell, "triple":triple, "detail":why
                })
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
            let (report, begun_at, machine, cpu) = staged(
                ctx,
                &quiet,
                spec,
                held,
                seams,
                || {
                    let event = ctx.record_event(&report::stage_started(i, &spec.title));
                    if let Ok(mut o) = out.lock() {
                        let _ = o.write_all(event.as_bytes()).and_then(|()| o.flush());
                    }
                },
                || {
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
                },
            );
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
    // Only the shim's cache has a verification cache in it ([`Ctx::cached_lanes_dir`]).
    let shared = ctx.cached_lanes_dir.is_some();
    match (&ctx.main_build_dir, shared) {
        (Some(value), true) if value.contains(&format!("/{}/", aterm_tempfile::QUIET)) => {
            s.push_str(&format!(
                "build cache: the main lane shares plain targo's ({value}), its target {}\n",
                ctx.target_dir.display()
            ));
        }
        (Some(value), true) => s.push_str(&format!(
            "build cache: the main lane shares plain targo's ({value}), its target {}, outside \
             .ubd: the targo shim on PATH predates the quiet cache, and an aterm update lays the \
             new one\n",
            ctx.target_dir.display()
        )),
        (Some(value), false) => s.push_str(&format!(
            "build cache: CARGO_TARGET_DIR={}, intermediates out of Spotlight ({value}; no shim cache here)\n",
            ctx.target_dir.display()
        )),
        (None, _) => s.push_str(&format!(
            "build cache: CARGO_TARGET_DIR={} (no shim cache here; the caller chose the build directory)\n",
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
fn planned_checks(parsed: &cli::Args, scope: &Scope, root: &Path) -> (Vec<Check>, bool) {
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
    let cells_pulled = checks::pulls_cells(&wanted, parsed.release, !scope.selects_nothing());
    if cells_pulled {
        wanted = checks::with(&wanted, Check::Cells);
    }
    (wanted, cells_pulled)
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
        .with_halt(halt_of(parsed.release, parsed.fail_fast))
        .with_cursor_pacing(parsed.cursor_pacing)
        .with_compile_only(parsed.compile_only)
        .with_measure_artifact(measure_artifact)
        .with_child_ceiling(parsed.timeout.unwrap_or(Some(exec::DEFAULT_CHILD_CEILING)))
        .with_pool(pool)
        .with_test_threads(parsed.test_threads)
        .with_test_cores(parsed.test_cores)
        .with_passed_binaries(passed_binaries)
        .with_passed_lint_packages(passed_lint_packages)
        .with_carry_context(parsed.carry_context.clone())
        .with_events(events)
        .with_quiet(Some(if parsed.background {
            aterm_capacity::Admit::Preemptible
        } else {
            parsed.admit
        }))
        .with_memory(memory::Book::live())
        .with_test_history(
            aterm_types::dirs::account_state_dir()
                .map(|d| d.join(aterm_capacity::book::TEST_HISTORY)),
        )
        .with_toolchain_lease(lease_said)
        .with_lock_notes(lock_said)
        .with_quiet_tmpdir(&std::env::temp_dir())
        .with_head_at_start(head_at_start);
    ctx.background = parsed.background;
    ctx.test_seed = parsed.test_seed;
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
        &ctx.state_dir().join("logs.noindex"),
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
        "verify-event: {\"schema\":1,\"event\":\"invocation-started\"}",
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

/// A private scratch directory named for `tag`, made atomically. On Unix it is `mktemp -d`'s,
/// under `/tmp`, which keeps unix-socket paths under macOS's 104-byte `sockaddr_un` limit;
/// elsewhere (Windows has no `mktemp`) it is [`aterm_tempfile`]'s, in the temporary directory.
/// The caller removes it.
///
/// # Errors
/// Fails when `mktemp` is absent or refuses to create the directory, or, off Unix, when the
/// temporary directory refuses it.
pub fn mktemp_dir(tag: &str) -> std::io::Result<PathBuf> {
    if !cfg!(unix) {
        return Ok(aterm_tempfile::Builder::new()
            .prefix(&format!("{tag}."))
            .tempdir()?
            .keep());
    }
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("mktemp"))
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
mod tests {
    use super::*;

    #[test]
    fn the_production_planner_pulls_forge_only_for_a_dev_touch_of_a_fork() {
        let manifest_dir = aterm_tempfile::manifest_dir();
        let root = manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("aterm-verify is two directories below the workspace root");
        let parsed = |words: &[&str]| {
            let parsed = cli::parse(words.iter().copied()).expect("the invocation parses");
            let scope = Scope::packages(&parsed.packages);
            planned_checks(&parsed, &scope, root).0
        };

        let fork = parsed(&["-p", "aterm-window"]);
        assert!(fork.contains(&Check::Forge), "default dev plan: {fork:?}");
        assert!(
            checks::stages_of(&fork).contains(&plan::StageId::Forge),
            "`verify -p aterm-window` must carry the Forge stage that runs OB-7: {fork:?}"
        );
        let unrelated = parsed(&["-p", "aterm-types"]);
        assert!(
            !unrelated.contains(&Check::Forge),
            "an unrelated package does not pay for Forge: {unrelated:?}"
        );
        let explicit_lint = parsed(&["lint", "-p", "aterm-window"]);
        assert!(
            !explicit_lint.contains(&Check::Forge),
            "an explicitly narrow non-dev invocation stays exact: {explicit_lint:?}"
        );

        let dev = cli::parse(std::iter::empty::<&str>()).expect("default invocation parses");
        let vendored_seed = Scope::changed("origin/main", vec!["aterm-gui".into()], true)
            .with_seeds(vec!["aterm-window".into()]);
        let changed = planned_checks(&dev, &vendored_seed, root).0;
        assert!(
            changed.contains(&Check::Forge),
            "a touched vendored seed need not itself be a workspace cone member: {changed:?}"
        );
        let dependent_only = Scope::changed(
            "origin/main",
            vec!["aterm-window".into(), "aterm-gui".into()],
            true,
        )
        .with_seeds(vec!["aterm-gui".into()]);
        let dependent = planned_checks(&dev, &dependent_only, root).0;
        assert!(
            !dependent.contains(&Check::Forge),
            "a fork merely present in the dependent cone was not touched: {dependent:?}"
        );
    }

    #[test]
    fn background_runs_admit_their_disk_as_background() {
        assert_eq!(
            run_admit_mode(true, aterm_capacity::Admit::Wait),
            aterm_capacity::Admit::Preemptible
        );
        assert_eq!(
            run_admit_mode(false, aterm_capacity::Admit::Wait),
            aterm_capacity::Admit::Wait,
            "negative control: an ordinary verifier keeps its requested mode"
        );
        assert_eq!(
            run_admit_mode(false, aterm_capacity::Admit::Now),
            aterm_capacity::Admit::Now,
            "negative control: --admit now remains an ordinary override"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scratch_permission_changes_refuse_links_and_regular_files() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let root = aterm_tempfile::tempdir().unwrap();
        let directory = root.path().join("directory");
        std::fs::create_dir(&directory).unwrap();
        set_dir_mode_nofollow(&directory, 0o700).unwrap();
        let link = root.path().join("link");
        symlink(&directory, &link).unwrap();
        assert!(set_dir_mode_nofollow(&link, 0o777).is_err());
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        let file = root.path().join("file");
        std::fs::write(&file, b"untouched").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(set_dir_mode_nofollow(&file, 0o777).is_err());
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
            0o600
        );
    }

    /// A child's captured output past the cap on the files verify reads is read whole (a sparse
    /// file, so the test writes no 128 MiB to disk).
    #[test]
    fn a_childs_output_is_read_whole_past_the_file_cap() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let out = dir.path().join("stdout");
        std::fs::File::create(&out)
            .unwrap()
            .set_len(MAX_VERIFY_FILE_BYTES + 1)
            .unwrap();
        assert!(read_regular(&out).is_err(), "an input file past the cap");
        let read = read_captured(&out).expect("a child's output past the cap");
        assert_eq!(
            u64::try_from(read.len()).unwrap(),
            MAX_VERIFY_FILE_BYTES + 1
        );
    }

    #[cfg(unix)]
    #[test]
    fn child_tools_find_native_packages_after_the_callers_choices() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let caller = dir.path().join("caller");
        let native = dir.path().join("native");
        standin(&caller.join("oracle"), "printf caller");
        standin(&native.join("bin/oracle"), "printf native");
        let ctx = Ctx::new(
            dir.path().into(),
            Scope::workspace(),
            EnvSnapshot {
                path: caller.as_os_str().to_owned(),
                native_packages: Some(native),
                ..EnvSnapshot::default()
            },
            dir.path().into(),
        );
        let run = || {
            exec::run(
                &exec::Cmd::new(aterm_uds::os_path!("sh")).args(["-c", "oracle"]),
                ctx.exec_env(),
            )
        };
        let first = run();
        assert!(first.ok, "{}", first.output);
        assert_eq!(first.output, "caller");
        std::fs::remove_file(caller.join("oracle")).unwrap();
        let second = run();
        assert!(second.ok, "{}", second.output);
        assert_eq!(second.output, "native");
        assert_eq!(
            native_package_path(caller.as_os_str(), None),
            caller.as_os_str(),
            "an absent or disabled native prefix adds no search path"
        );
    }

    /// A fixture going removes no name of the file another fixture's stand-in of the same text
    /// runs ([`standin`]): that file has one name, outside every fixture, and the stand-in
    /// still reads its body beside its own path.
    #[cfg(unix)]
    #[test]
    fn a_fixture_going_leaves_the_file_another_standin_runs_its_one_name() {
        use std::os::unix::fs::MetadataExt as _;
        let gone = mktemp_dir("atv-standin-gone").expect("mktemp");
        let kept = mktemp_dir("atv-standin-kept").expect("mktemp");
        standin_body(&gone.join("tool"), "exit 6");
        standin_body(&kept.join("tool"), "exit 7");
        std::fs::remove_dir_all(&gone).expect("the first fixture goes");
        let ran = aterm_uds::spawnfd::command(kept.join("tool"))
            .status()
            .map(|s| s.code());
        let runs = std::fs::canonicalize(kept.join("tool")).expect("the stand-in resolves");
        let fixture = std::fs::canonicalize(&kept).expect("the fixture resolves");
        let names = std::fs::metadata(&runs).map(|m| m.nlink());
        std::fs::remove_dir_all(&kept).expect("the second fixture goes");
        assert_eq!(ran.ok(), Some(Some(7)), "the stand-in ran its own body");
        assert!(
            !runs.starts_with(&fixture),
            "{} runs from inside its fixture",
            runs.display()
        );
        assert_eq!(
            names.ok(),
            Some(1),
            "{} has names a fixture removes",
            runs.display()
        );
    }

    #[test]
    fn a_borrowed_tools_build_is_held_for_the_run() {
        #[cfg(unix)]
        if !aterm_uds::test_binary::isolated_lifecycle(
            "tests::a_borrowed_tools_build_is_held_for_the_run",
        ) {
            return;
        }
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
            lock: None,
            lock_refusal: None,
            lock_override: None,
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
        #[cfg(unix)]
        let fork = aterm_uds::fork_window::Parked::park();
        drop(held);
        #[cfg(unix)]
        {
            // SDK leases deliberately retain genuine descriptor duplicates. A sibling
            // spawn also holds one until exec, even though this run dropped its lease.
            assert!(matches!(
                lease::holders(&prefix, &subject),
                Holders::Held(_)
            ));
            assert!(fork.release().success());
        }
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the final borrowed-tool lease descriptor closing",
            || match lease::holders(&prefix, &subject) {
                Holders::Free => Some(()),
                Holders::Held(_) => None,
                Holders::Unknown(error) => panic!("lease probe failed: {error}"),
            },
        );

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
                None,
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

    /// A run's children get the temp directory's quiet folder as `TMPDIR` on macOS. The loud
    /// directory is a private scratch one, not `/tmp`, whose shared quiet folder another program
    /// or user may have made in a form this one refuses (verify passes `std::env::temp_dir()`).
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
        let loud = aterm_tempfile::Builder::new()
            .heard()
            .tempdir()
            .expect("a loud scratch directory");
        let got = tmpdir(&ctx().with_quiet_tmpdir(loud.path()));
        if cfg!(target_os = "macos") {
            let got = got.expect("a quiet TMPDIR");
            assert_eq!(got, loud.path().join(".ubd"));
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
            let cache = disk::build_dir_path(got.as_deref().unwrap(), &root).unwrap();
            assert_eq!(plan::lanes_dir(&c), cache.join("verify/.ubd"));
            assert!(plan::lanes_dir(&c).starts_with(&cache));
            assert!(!plan::lanes_dir(&c).starts_with(&root));
            let disk = disk::run_of(&c).unwrap();
            assert!(disk.dirs.contains(&cache));
            let lane = plan::lane_dir(&c, plan::Lane::TippyTarget).unwrap();
            std::fs::create_dir_all(&lane).unwrap();
            std::fs::write(lane.join("retained-artifact"), b"cached").unwrap();
            std::fs::remove_dir(&root).unwrap();
            std::fs::create_dir_all(&root).unwrap();
            let recreated = Ctx::new(
                root.clone(),
                Scope::workspace(),
                env.clone(),
                PathBuf::from("/tmp"),
            );
            let retained = plan::lane_dir(&recreated, plan::Lane::TippyTarget).unwrap();
            assert_eq!(retained, lane);
            assert_eq!(
                std::fs::read(retained.join("retained-artifact")).unwrap(),
                b"cached"
            );
            let other = home.join("Developer/other");
            std::fs::create_dir_all(&other).unwrap();
            let other = Ctx::new(
                other,
                Scope::workspace(),
                env.clone(),
                PathBuf::from("/tmp"),
            );
            assert_ne!(plan::lane_dir(&other, plan::Lane::TippyTarget), Some(lane));
            assert_eq!(
                c.child_env_add
                    .iter()
                    .find(|(k, _)| k == "CARGO_TARGET_DIR")
                    .map(|(_, v)| PathBuf::from(v)),
                Some(root.join("target")),
                "pinned beside the shim's cache, which the main lane names by --config"
            );
            assert_eq!(c.target_dir, root.join("target"));
            for var in &CARGO_LAYOUT_VARS[..3] {
                let chosen = EnvSnapshot {
                    cargo_layout: vec![(*var, OsString::new())],
                    ..env.clone()
                };
                assert_eq!(main_build_dir(&chosen, &root), None, "{var}");
                let selected = Ctx::new(
                    root.clone(),
                    Scope::workspace(),
                    chosen,
                    PathBuf::from("/tmp"),
                );
                assert_eq!(selected.cached_lanes_dir, None, "{var}");
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

    /// A checkout whose target is a link (a release clone's `target -> target.noindex`), itself
    /// under a linked folder (`/tmp`): every child is handed the physical target, so cargo spells
    /// each `CARGO_BIN_EXE_*` it compiles into a test (`<CARGO_TARGET_DIR>/debug/<bin>`, never
    /// resolved) as verify's own paths are. NEGATIVE CONTROL: without [`physical_target`] the
    /// target keeps the link's spelling, and without the pin a shim-mode child gets none.
    #[cfg(unix)]
    #[test]
    fn every_child_builds_through_the_physical_target_however_the_checkout_reaches_it() {
        let tmp = mktemp_dir("atv-physical").expect("mktemp");
        // HOME is physical, so the shim's cache holds the checkout; the root is handed spelled
        // through a link of its own (and, on macOS, the one `/tmp` is).
        let home = std::fs::canonicalize(&tmp).expect("real").join("home");
        std::fs::create_dir_all(home.join("Library/Caches")).expect("mkdir");
        std::os::unix::fs::symlink("home", tmp.join("linked-home")).expect("link");
        let root = tmp.join("linked-home/Developer/repo");
        std::fs::create_dir_all(root.join("target.noindex")).expect("mkdir");
        std::os::unix::fs::symlink("target.noindex", root.join("target")).expect("link");
        let real_root = std::fs::canonicalize(&root).expect("real");
        let physical = real_root.join("target.noindex");
        let pinned = |c: &Ctx| {
            c.child_env_add
                .iter()
                .find(|(k, _)| k == "CARGO_TARGET_DIR")
                .map(|(_, v)| PathBuf::from(v))
        };
        let shim = EnvSnapshot {
            home: home.clone(),
            ..EnvSnapshot::default()
        };
        let linked = root.join("target");
        let theirs = EnvSnapshot {
            cargo_target_dir: Some(linked.clone()),
            cargo_layout: vec![("CARGO_TARGET_DIR", linked.into_os_string())],
            ..EnvSnapshot::default()
        };
        let shares = cfg!(target_os = "macos");
        // HOME spelled through the link holds no physical checkout, so no shim cache, as the
        // shim decides from its own physical working directory: the target is pinned all the same.
        let linked_home = EnvSnapshot {
            home: tmp.join("linked-home"),
            ..EnvSnapshot::default()
        };
        for (env, cache) in [
            (shim, shares),
            (linked_home, false),
            (EnvSnapshot::default(), false),
            (theirs, false),
        ] {
            let c = Ctx::new(
                root.clone(),
                Scope::workspace(),
                env.clone(),
                PathBuf::from("/tmp"),
            );
            assert_eq!(c.cached_lanes_dir.is_some(), cache, "{env:?}");
            assert_eq!(c.root, real_root, "{env:?}");
            assert_eq!(c.target_dir, physical, "{env:?}");
            assert_eq!(pinned(&c), Some(physical.clone()), "{env:?}");
            assert_eq!(stages::main_bin(&c, "aterm"), physical.join("debug/aterm"));
            assert!(c.state_dir().starts_with(&physical), "{env:?}");
            let lanes = plan::lanes_dir(&c);
            assert_eq!(lanes, crate::physical(&lanes), "{env:?}");
        }
        std::fs::remove_dir_all(&tmp).expect("cleanup");
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
        let fast = std::path::absolute("/fast/tgt").expect("absolute");
        let theirs = EnvSnapshot {
            cargo_target_dir: Some(fast.clone()),
            cargo_layout: vec![("CARGO_TARGET_DIR", fast.clone().into_os_string())],
            ..EnvSnapshot::default()
        };
        let repo = std::path::absolute("/repo").expect("absolute");
        let c = ctx(&repo, theirs.clone());
        let fast_cfg = format!("build.build-dir='{}'", fast.join("build.noindex").display());
        assert_eq!(pinned(&c), Some(fast));
        assert_eq!(c.main_build_dir.as_deref(), Some(fast_cfg.as_str()));
        let argv = stages::main_targo(&c, vec!["--unverified".into(), "test".into()]).argv();
        let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
        assert_eq!(
            argv[1..],
            [
                "--config",
                fast_cfg.as_str(),
                "--unverified",
                config,
                workspace,
                "test",
                aterm_toolchain::build_cache::TEST_GRAPH_FLAG
            ]
        );
        let c = ctx(&repo, EnvSnapshot::default());
        let repo_cfg = format!(
            "build.build-dir='{}'",
            repo.join("target").join("build.noindex").display()
        );
        assert_eq!(pinned(&c), Some(repo.join("target")));
        assert_eq!(c.main_build_dir.as_deref(), Some(repo_cfg.as_str()));
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
    fn requested_test_cores_narrow_but_never_grant_a_share() {
        let make = |env| {
            Ctx::new(
                PathBuf::from("/repo"),
                Scope::workspace(),
                env,
                PathBuf::from("/tmp"),
            )
        };
        let held = || {
            make(EnvSnapshot {
                build_slot_held: Some("1".into()),
                cargo_build_jobs: Some("6".into()),
                ..EnvSnapshot::default()
            })
        };
        let capped = held().with_test_cores(std::num::NonZeroU32::new(3));
        assert_eq!(capped.test_share(), 3);
        assert_eq!(capped.test_thread_override(), Some(3));
        assert_eq!(
            held()
                .with_test_cores(std::num::NonZeroU32::new(20))
                .test_share(),
            6
        );
        let unheld = make(EnvSnapshot::default()).with_test_cores(std::num::NonZeroU32::new(1));
        assert_eq!(unheld.test_share(), 1);
        assert_eq!(
            test_workers::share(&unheld.env, None),
            Ok(test_workers::Share::Claim(None))
        );
        let malformed = make(EnvSnapshot {
            build_slot_held: Some("1".into()),
            ..EnvSnapshot::default()
        })
        .with_test_cores(std::num::NonZeroU32::new(20));
        assert_eq!(malformed.test_share(), 1);
        assert!(test_workers::share(&malformed.env, None).is_err());
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

    /// A red narrowed cell's rerun judges that crate again, not the whole graph and the oracles.
    #[test]
    fn a_cells_rerun_keeps_the_p_that_narrowed_it() {
        let c = |scope| {
            Ctx::new(
                PathBuf::from("/repo"),
                scope,
                EnvSnapshot::default(),
                PathBuf::from("/tmp"),
            )
        };
        let narrowed = c(Scope::packages(&["aterm-agent".to_string()]));
        assert!(narrowed.rerun(Check::Cells).contains(" -p aterm-agent"));
        let whole = c(Scope::workspace());
        assert!(!whole.rerun(Check::Cells).contains(" --all"));
        assert!(!whole.rerun(Check::Cells).contains(" -p "));
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

    /// A release's header names the half it runs, or the checks `--checks` listed, so its log
    /// says which part of the release it answers for.
    #[test]
    fn a_release_header_names_its_half() {
        let head = |words: &[&str]| {
            let a = cli::parse(words.iter().copied()).expect("parses");
            let ctx = Ctx::new(
                PathBuf::from("/repo"),
                Scope::workspace(),
                EnvSnapshot::default(),
                PathBuf::from("/tmp"),
            )
            .with_checks(&a.wanted(|_| true))
            .with_release(a.release);
            header(&ctx, None)
        };
        let starts = |words: &[&str], what: &str| {
            let line = head(words);
            assert!(line.starts_with(&format!("verify: {what} — ")), "{line}");
        };
        starts(&["release", "--app", "a"], "release");
        starts(&["release", "--tree"], "release --tree");
        starts(&["release", "--bundle", "--app", "a"], "release --bundle");
        starts(&["release", "--checks", "test,spec"], "release: test spec");
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
        assert_eq!(head(&["aterm-gpu"]), "verify: dev — the cone");
        assert_eq!(
            head(&["aterm-core", "aterm-gpu", "aterm-gpu-web", "aterm-wasm"]),
            "verify: dev + wasm-smoke (the selection holds aterm-wasm) — the cone"
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
            "verify: dev + cells (each other OS's cell and one of this OS's that builds a selected crate) — the cone"
        );
        assert_eq!(
            pulled(&["aterm-wasm"]),
            "verify: dev + wasm-smoke (the selection holds aterm-wasm) + cells (each other OS's cell and one of this OS's that builds a selected crate) — the cone"
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

    #[test]
    fn a_completed_stage_is_reported_before_its_dependent_runs() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static REPORTED: AtomicBool = AtomicBool::new(false);
        static STARTED: AtomicBool = AtomicBool::new(false);
        static DEPENDENT_RAN: AtomicBool = AtomicBool::new(false);
        struct LiveOutput;
        impl std::io::Write for LiveOutput {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(json) = bytes.strip_prefix(b"verify-event: ") {
                    let event: aterm_json::Value = aterm_json::from_slice(json).unwrap();
                    match event["event"].as_str() {
                        Some("stage-started") => STARTED.store(true, Ordering::SeqCst),
                        Some("stage-finished") => REPORTED.store(true, Ordering::SeqCst),
                        _ => {}
                    }
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        fn stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
            assert!(
                STARTED.load(Ordering::SeqCst),
                "stage start was not reported"
            );
            if spec.id != plan::StageId::TestCompile {
                assert!(
                    REPORTED.load(Ordering::SeqCst),
                    "compile completion was buffered"
                );
                DEPENDENT_RAN.store(true, Ordering::SeqCst);
            }
            let mut report = Report::new(spec.title.clone());
            report.pass("ran");
            report
        }
        REPORTED.store(false, Ordering::SeqCst);
        STARTED.store(false, Ordering::SeqCst);
        DEPENDENT_RAN.store(false, Ordering::SeqCst);
        let ctx = Ctx::new(
            PathBuf::from("/repo"),
            Scope::crate_only("aterm-verify"),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .with_checks(&[Check::Test]);
        let result = run_with(
            &ctx,
            &mut LiveOutput,
            &mut std::io::sink(),
            &Show::default(),
            Seams {
                stage,
                prepare_cells: None,
                ..LIVE
            },
        );
        assert_eq!(result.unwrap(), exit::PASS);
        assert!(REPORTED.load(Ordering::SeqCst));
        assert!(DEPENDENT_RAN.load(Ordering::SeqCst));
    }

    #[test]
    fn a_private_companion_refuses_public_aliases_and_keeps_internal_producer_records() {
        fn stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
            let mut report = Report::new(spec.title.clone());
            report.pass("ran");
            report
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("chosen.log");
        let context = || {
            Ctx::new(
                dir.path().to_path_buf(),
                Scope::crate_only("aterm-verify"),
                EnvSnapshot::default(),
                dir.path().join("scratch"),
            )
            .with_checks(&[Check::Fmt])
        };
        let mut previous = context();
        let log = open_run_log(&mut previous, Some(&path), dir.path()).unwrap();
        drop(log);
        drop(previous);
        let alias = dir.path().join("external-alias.events");
        std::fs::hard_link(run_log::events_path(&path), &alias).unwrap();
        assert!(event_log::EventLog::open(&alias).is_err());
        assert!(event_log::EventLog::open(&run_log::events_path(&path)).is_err());
        let human_alias = dir.path().join("external-human-alias.log");
        std::fs::hard_link(&path, &human_alias).unwrap();
        assert!(event_log::EventLog::open(&path).is_err());
        assert!(event_log::EventLog::open(&human_alias).is_err());
        let mut ctx = context();
        let mut log = open_run_log(&mut ctx, Some(&path), dir.path()).unwrap();
        assert_eq!(
            run_with(
                &ctx,
                &mut Vec::new(),
                &mut log,
                &Show::default(),
                Seams {
                    stage,
                    prepare_cells: None,
                    ..LIVE
                }
            )
            .unwrap(),
            exit::PASS
        );
        drop(log);
        drop(ctx);
        assert!(
            std::fs::read(&path)
                .unwrap()
                .starts_with(run_log::PRIVATE_HUMAN_HEADER.as_bytes())
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            std::fs::read(&human_alias).unwrap()
        );
        assert!(event_log::EventLog::open(&human_alias).is_err());
        let text = std::fs::read_to_string(&alias).unwrap();
        assert_eq!(
            std::fs::read(&alias).unwrap(),
            std::fs::read(run_log::events_path(&path)).unwrap()
        );
        let observed = status::observe(&text);
        assert!(text.starts_with(run_log::PRIVATE_EVENTS_HEADER));
        assert_eq!(observed.malformed_complete_records, 0);
        assert_eq!(observed.invocations.len(), 1);
        assert_eq!(observed.invocations[0].observed.terminal_records, 1);
        assert_eq!(
            observed.invocations[0].observed.terminal_consistency,
            "matching-observed-rows"
        );
        assert_eq!(observed.invocations[0].observed.checks[0].occurrences, 1);
        assert_eq!(
            text.lines()
                .filter(|line| line.contains("invocation-started"))
                .count(),
            1
        );
    }

    #[test]
    fn structured_records_survive_the_external_temp_directory_and_keep_actual_producer_bytes() {
        fn stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
            let mut report = Report::new(spec.title.clone());
            report.pass("ran the bounded fixture stage");
            report
        }
        let retained = aterm_tempfile::tempdir().unwrap();
        let transient = aterm_tempfile::tempdir().unwrap();
        let external = transient.path().join("verify.events");
        let mut ctx = Ctx::new(
            retained.path().to_path_buf(),
            Scope::crate_only("aterm-verify"),
            EnvSnapshot::default(),
            retained.path().join("scratch"),
        )
        .with_checks(&[Check::Fmt])
        .with_events(Some(event_log::EventLog::open(&external).unwrap()));
        let path = retained.path().join("kept.log");
        let mut log = open_run_log(&mut ctx, Some(&path), retained.path()).unwrap();
        let id = ctx.events.as_ref().unwrap().id().unwrap().to_owned();
        let mut out = Vec::new();
        let code = run_with(
            &ctx,
            &mut out,
            &mut log,
            &Show::default(),
            Seams {
                stage,
                prepare_cells: None,
                ..LIVE
            },
        )
        .unwrap();
        assert_eq!(code, exit::PASS);
        #[cfg(unix)]
        let_disk_go(
            disk::Disk::Unclaimed,
            true,
            Some(&mut log),
            ctx.events.as_ref(),
        );
        drop(log);
        drop(ctx);
        let external_bytes = std::fs::read(&external).unwrap();
        drop(transient);
        assert!(!external.exists());
        let at = run_log::events_path(&path);
        let kept = std::fs::read(&at).unwrap();
        assert_eq!(
            kept.strip_prefix(run_log::PRIVATE_EVENTS_HEADER.as_bytes())
                .unwrap(),
            external_bytes
        );
        let text = String::from_utf8(kept).unwrap();
        let observed = status::observe(&text);
        assert_eq!(observed.malformed_complete_records, 0);
        assert_eq!(observed.invocations.len(), 1);
        let run = &observed.invocations[0];
        assert_eq!(run.id, id);
        assert!(run.source_observation.is_some());
        assert_eq!(run.observed.terminal_consistency, "matching-observed-rows");
        let rows = text
            .lines()
            .filter_map(event_log::CheckRow::of_event)
            .collect();
        let finished = text
            .lines()
            .rev()
            .find_map(event_log::Finished::of_event)
            .unwrap();
        assert!(
            event_log::Judged {
                code: finished.code,
                checks: rows,
                secs: 0,
                finished: Some(finished)
            }
            .whole()
        );
        let output = String::from_utf8(out).unwrap();
        for line in output
            .lines()
            .filter(|line| line.starts_with("verify-event: "))
        {
            assert!(
                text.lines().any(|kept| kept == line),
                "same prepared bytes in both sinks"
            );
        }
        let kinds = text
            .lines()
            .map(|line| {
                aterm_json::from_str::<aterm_json::Value>(
                    line.strip_prefix("verify-event: ").unwrap(),
                )
                .unwrap()["event"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert!(kinds.iter().any(|kind| kind == "stage-started"));
        assert!(kinds.iter().any(|kind| kind == "stage-finished"));
        #[cfg(unix)]
        assert!(
            kinds
                .iter()
                .position(|kind| kind == "run-finished")
                .unwrap()
                < kinds.iter().position(|kind| kind == "disk").unwrap()
        );
        assert_eq!(
            status::run(
                &[
                    "--events".into(),
                    at.to_string_lossy().into_owned(),
                    "--json".into()
                ],
                retained.path()
            ),
            exit::PASS
        );
    }

    /// Each check's `check-finished` event is written once, whole, right before its own line and
    /// before the verdict, with the word that line opens with, and the same line to the run's
    /// events file, which a release's cutter keeps one record per check from (A19); the file
    /// ends with the run's `run-finished` line, naming its exit and each check's word, by which
    /// `verify land` knows the record whole (B10). Negative controls: emitting the event after
    /// the rendered line fails the order assertion; one never recorded leaves the events file
    /// without it.
    #[test]
    fn each_check_says_check_finished_once_before_its_line_and_the_verdict() {
        fn stage(_: &Ctx, spec: &plan::StageSpec) -> Report {
            let mut report = Report::new(spec.title.clone());
            if spec.id == plan::StageId::Census {
                report.fail("a guard found a banned call");
            } else {
                report.pass("ran");
            }
            report
        }
        let ctx = Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .with_checks(&[Check::Fmt, Check::Census]);
        let dir = aterm_tempfile::tempdir().unwrap();
        let at = dir.path().join("06-verify.events");
        let ctx = ctx.with_events(Some(event_log::EventLog::open(&at).unwrap()));
        let mut out: Vec<u8> = Vec::new();
        let code = run_with(
            &ctx,
            &mut out,
            &mut std::io::sink(),
            &Show::default(),
            Seams {
                stage,
                prepare_cells: None,
                ..LIVE
            },
        )
        .unwrap();
        assert_eq!(code, exit::FAILED);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let verdict = lines
            .iter()
            .position(|l| l.starts_with("verify: FAIL"))
            .expect("a verdict line");
        for (name, word) in [("fmt", "ok"), ("census", "FAIL")] {
            let events: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter_map(|(i, l)| {
                    let json = l.strip_prefix("verify-event: ")?;
                    let event: aterm_json::Value = aterm_json::from_slice(json.as_bytes()).ok()?;
                    (event["event"].as_str() == Some("check-finished")
                        && event["check"].as_str() == Some(name))
                    .then(|| {
                        assert_eq!(event["outcome"].as_str(), Some(word), "{l}");
                        i
                    })
                })
                .collect();
            assert_eq!(events.len(), 1, "{name}: {text}");
            let own = lines
                .iter()
                .position(|l| l.starts_with(word) && l.split_whitespace().nth(1) == Some(name))
                .expect("the check's own line");
            assert_eq!(
                events[0] + 1,
                own,
                "{name}'s event comes right before its line: {text}"
            );
            assert!(own < verdict, "{text}");
        }
        let said: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| {
                l.strip_prefix("verify-event: ")
                    .and_then(|json| aterm_json::from_str::<aterm_json::Value>(json).ok())
                    .is_some_and(|event| event["event"].as_str() == Some("check-finished"))
            })
            .collect();
        let kept = std::fs::read_to_string(&at).unwrap();
        let drivers = kept
            .lines()
            .filter_map(|line| line.strip_prefix("verify-event: "))
            .filter_map(|json| aterm_json::from_str::<aterm_json::Value>(json).ok())
            .filter(|event| event["event"].as_str() == Some("measure-driver"))
            .collect::<Vec<_>>();
        assert_eq!(drivers.len(), 1);
        assert_eq!(drivers[0]["sha256"].as_str(), report::running_driver());
        let (last, checks) = kept
            .lines()
            .collect::<Vec<_>>()
            .split_last()
            .map(|(last, checks)| (last.to_string(), checks.to_vec()))
            .expect("the events file");
        let mut kept_checks = checks
            .iter()
            .copied()
            .filter(|line| event_log::CheckRow::of_event(line).is_some())
            .collect::<Vec<_>>();
        let mut said = said;
        kept_checks.sort_unstable();
        said.sort_unstable();
        assert_eq!(
            kept_checks, said,
            "exact records in both sinks, independent concurrent ordering: {kept}"
        );
        assert!(
            checks.iter().any(|line| line.contains("stage-started")),
            "actual stage records are retained too"
        );
        let finished: aterm_json::Value =
            aterm_json::from_str(last.strip_prefix("verify-event: ").expect("an event")).unwrap();
        assert_eq!(finished["event"].as_str(), Some("run-finished"), "{last}");
        assert_eq!(
            finished["code"].as_i64(),
            Some(i64::from(exit::FAILED)),
            "{last}"
        );
        let words: Vec<(&str, &str)> = finished["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .filter_map(|c| Some((c["check"].as_str()?, c["outcome"].as_str()?)))
            .collect();
        assert_eq!(words, [("fmt", "ok"), ("census", "FAIL")], "{last}");
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
                    prepare_cells: None,
                    admit_quiet: LIVE.admit_quiet,
                    admit_memory: LIVE.admit_memory,
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

    /// A stage the run's memory budget held back says so on its time line, naming for how long,
    /// behind which stages, and what it claims of the budget; one never held says nothing.
    /// NEGATIVE CONTROL: the same stage, not held, has no note.
    #[test]
    fn a_stage_held_for_memory_says_how_long_and_behind_what() {
        const GIB: u64 = 1 << 30;
        let stages: Vec<plan::StageSpec> = plan::declared(&Scope::crate_only("aterm-gui"))
            .into_iter()
            .filter(|s| matches!(s.id, plan::StageId::TestCompile | plan::StageId::Tippy))
            .map(|s| plan::StageSpec { mem: 12 * GIB, ..s })
            .collect();
        let held = sched::Held {
            waited: Duration::from_millis(41_250),
            behind: vec![0],
        };
        let note = held_note(&stages, 1, &held, 43 * GIB / 2).expect("held");
        assert_eq!(
            note,
            "memory: held 41.2s for room behind test compile (-p aterm-gui): it claims 12 GiB of \
             the run's 21.5 GiB"
        );
        assert_eq!(
            held_note(&stages, 1, &sched::Held::default(), 43 * GIB / 2),
            None
        );
    }

    #[test]
    fn built_from_rejects_short_or_non_hex_commits() {
        let art = |commit: &str| MeasureArtifact {
            app: PathBuf::from("/tmp/aterm.app"),
            version: Some("0.137.0".into()),
            commit: Some(commit.into()),
        };
        let head = "22cb722a2ec9abcdef0123456789abcdef012345";
        assert!(!art("22cb72").built_from(head), "6 hex chars is too short");
        assert!(art("22cb722").built_from(head), "7 hex chars is valid");
        assert!(!art("22cb722z").built_from("22cb722zabcdef0123456789abcdef0123456789"));
    }

    /// A complete store build `build` under `prefix` whose `trustc` answers `--print sysroot` and
    /// `-vV` (commit `commit`) and whose `targo` runs: its tree's fold.
    #[cfg(unix)]
    fn locked_build(prefix: &Path, build: u64, commit: &str) -> String {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = atpkg::trust_lock::build_dir(prefix, build);
        std::fs::create_dir_all(dir.join("lib/rustlib")).expect("rustlib");
        std::fs::write(atpkg::trust_lock::ready_marker(prefix, build), "ok\n").expect("ready");
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        for (tool, body) in [
            ("targo", "exit 0".to_string()),
            (
                "trustc",
                format!(
                    "case \"$1\" in -vV) printf 'commit-hash: {commit}\\n' ;; *) echo '{}' ;; esac",
                    dir.display()
                ),
            ),
        ] {
            let path = dir.join("bin").join(tool);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("stand-in");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        // A store build's `rustc` is a hard link of its `trustc`.
        std::fs::hard_link(dir.join("bin/trustc"), dir.join("bin/rustc")).expect("rustc");
        atpkg::tree::tree_root(&dir).expect("fold")
    }

    /// A LOCKED CHECKOUT'S RUN COMPILES WITH THE LOCK'S BUILD (A14): the store's `current` names
    /// another build, and the run takes the lock's, folding and recording a build installed before
    /// records; it says so in its log, and says a `targo` shim laid for `current` makes plain
    /// builds and the run alternate units; a `RUSTC`, `CARGO_BUILD_RUSTC` or `build.rustc` whose
    /// compiler is not the lock's refuses the run's preflight. NEGATIVE CONTROL: without the lock
    /// the run takes `current`, says no lock, notes nothing and takes any `RUSTC`.
    #[cfg(unix)]
    #[test]
    fn a_locked_checkout_runs_the_locked_build_and_says_a_shim_that_does_not() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let home = scratch.path().canonicalize().expect("canonical");
        let env = EnvSnapshot {
            home: home.clone(),
            xdg_config_home: Some(home.join("xdg")),
            ..EnvSnapshot::default()
        };
        let prefix = toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref());
        let commit = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";
        let root_of_locked = locked_build(&prefix, 16387, commit);
        locked_build(&prefix, 16500, &"b".repeat(40));
        std::os::unix::fs::symlink(
            atpkg::trust_lock::build_dir(&prefix, 16500),
            prefix.join("store/trust/current"),
        )
        .expect("current");
        std::fs::create_dir_all(prefix.join("bin")).expect("shims");
        std::fs::write(
            prefix.join("bin/targo"),
            format!(
                "#!/bin/sh\n# atpkg shim\nexec '{}' \"$@\"\n",
                atpkg::trust_lock::build_dir(&prefix, 16500)
                    .join("bin/targo")
                    .display()
            ),
        )
        .expect("a shim laid for current");
        let checkout = home.join("checkout");
        std::fs::create_dir_all(&checkout).expect("checkout");
        std::fs::write(
            checkout.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"trust\"\n",
        )
        .expect("pin");

        let unlocked = run_toolchain(&env, &checkout);
        assert_eq!(
            unlocked.stage2_dir,
            atpkg::trust_lock::build_dir(&prefix, 16500).join("bin"),
            "no lock: current"
        );
        assert_eq!(lock_line(&unlocked), None);
        assert!(lock_notes(&env, &unlocked).is_empty());

        let lock = atpkg::trust_lock::TrustLock {
            build: 16387,
            tree_root: root_of_locked,
            compiler_commit: commit.to_string(),
            receipt: "SEED-MINT sha256:00".to_string(),
            stock_toolchain: "1.99.1".to_string(),
        };
        std::fs::write(
            checkout.join(atpkg::trust_lock::LOCK_FILE),
            atpkg::trust_lock::render_body(&lock),
        )
        .expect("lock");
        let locked = run_toolchain(&env, &checkout);
        assert!(locked.have_targo(), "{}", locked.missing_targo_label());
        assert_eq!(
            locked.stage2_dir,
            atpkg::trust_lock::build_dir(&prefix, 16387).join("bin")
        );
        assert!(
            lock_line(&locked).is_some_and(|l| l.contains("names Trust build 16387")),
            "{:?}",
            lock_line(&locked)
        );
        let notes = lock_notes(&env, &locked);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with("plain `targo` resolves 16500"),
            "{notes:?}"
        );

        // What names another compiler reaches every compile child: only the lock's compiler
        // passes the run's preflight, wherever it is named, resolved as the child resolves it.
        let trustc_of = |build| atpkg::trust_lock::build_dir(&prefix, build).join("bin/trustc");
        let with = |vars: &[(&'static str, &Path)]| EnvSnapshot {
            rustc_vars: vars
                .iter()
                .map(|(k, v)| (*k, v.display().to_string()))
                .collect(),
            ..env.clone()
        };
        let other = trustc_of(16500);
        let same = trustc_of(16387);
        assert!(preflight(&env, &checkout).is_ok(), "nothing names another");
        for var in ["RUSTC", "CARGO_BUILD_RUSTC"] {
            let refused = preflight(&with(&[(var, &other)]), &checkout).expect_err(var);
            assert!(
                refused.contains(&format!("commit {}", "b".repeat(40)))
                    && refused.contains(&format!("Fix: unset {var}")),
                "{refused}"
            );
            assert!(
                preflight(&with(&[(var, &same)]), &checkout).is_ok(),
                "{var}"
            );
        }
        assert!(
            preflight(&with(&[("RUSTC", Path::new("rustc"))]), &checkout).is_ok(),
            "a bare name is the child's PATH's, whose first entry is the locked build"
        );
        let config = checkout.join(".cargo/config.toml");
        std::fs::create_dir_all(checkout.join(".cargo")).expect(".cargo");
        std::fs::write(
            &config,
            format!("[build]\nrustc = \"{}\"\n", other.display()),
        )
        .expect("config");
        let refused = preflight(&env, &checkout).expect_err("build.rustc in the checkout's config");
        assert!(
            refused.contains(&format!(
                "Fix: remove build.rustc from {}",
                config.display()
            )),
            "{refused}"
        );
        assert!(
            preflight(&with(&[("RUSTC", &same)]), &checkout).is_ok(),
            "RUSTC outranks build.rustc"
        );
        std::fs::remove_file(&config).expect("rm config");
        std::fs::remove_file(checkout.join(atpkg::trust_lock::LOCK_FILE)).expect("rm lock");
        assert!(
            preflight(&with(&[("RUSTC", &other)]), &checkout).is_ok(),
            "no lock: RUSTC is the caller's"
        );
    }
}
