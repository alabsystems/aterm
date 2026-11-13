// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release's one build (`artifact.rs` keeps what it makes).
// STOCK EXCEPTION: the compat slice builds with `RUSTUP_TOOLCHAIN=stable`, because Trust has no x86_64-apple-darwin std in the aarch64 sysroot (`targo --unverified check --target x86_64-apple-darwin -p aterm` fails with E0463).
use aterm_uds::spawnfd::{Command, command};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::snapshot;

#[cfg(all(test, target_os = "macos"))]
#[path = "qualify_hop_measure.rs"]
mod hop_measure;

#[path = "linux_release.rs"]
pub mod linux;

const ARM64: &str = "aarch64-apple-darwin";
const X86_64: &str = "x86_64-apple-darwin";
const NATIVE_SLICE: &str = "native";
const COMPAT_CHECK_SLICE: &str = "x86_64-check";
const COMPAT_CHECK_EPOCH: u64 = 1;
const LIPO_ARM64: &str = "arm64";
const LIPO_X86_64: &str = "x86_64";

/// The cut's persistent log directory, supplied to its build child.
pub(crate) const SLICE_LOGS_ENV: &str = "ATERM_RELEASE_SLICE_LOGS";

/// Platform policy, independent of the bounded core allocation and per-slice memory admission.
pub enum Sequencing {
    /// Both macOS slices borrow from the build branch's private FIFO.
    Parallel,
    /// Linux's native build remains explicitly serial.
    OneAtATime,
}

/// Everything [`run`] needs, resolved by the caller.
pub struct BuildPlan {
    /// The checkout whose repository holds [`BuildPlan::commit`] — release orchestration runs here.
    pub repo_root: PathBuf,
    /// The full commit the release builds.
    pub commit: String,
    /// Receives what the build made, once the snapshot verified.
    pub out_dir: PathBuf,
    /// The reserved ledger number `n`.
    pub build_number: u64,
    /// Release version ("0.2.0") — names the dSYM zip `aterm-0.2.0-dSYM.zip`.
    pub short_version: String,
    /// Exact lowercase SHA-256 fingerprint that every independently-built and final shipped architecture slice must embed for its compiled updater key.
    pub expected_update_pin_sha256: Option<String>,
    /// Platform sequencing policy for architecture builds.
    pub sequencing: Sequencing,
    /// The shipped channel's published index run the build stamps into the binary's identity
    /// record ([`aterm_stamp::input::INDEX_HINT`]).
    pub index_hint: Option<String>,
}

/// What [`run`] produced.
pub struct BuildOutput {
    /// The unsigned bundle (`<out_dir>/aterm.app`).
    pub app: PathBuf,
    /// The snapshot digest the bytes were built from ([`snapshot::Snapshot::digest`]).
    pub snapshot_digest: String,
    /// Each slice's build ([`SliceBuild`]): its triple, its wall seconds, the sampled peak physical footprint, CPU and core wait of its build's descendants, and its units.
    pub slices: Vec<SliceBuild>,
    /// `lipo -archs` of the shipped `aterm` (such as "x86_64 arm64"), for the cut transcript.
    pub archs: String,
    /// The built binary's `--diagnose` `compiler:` line (`Trust <own version> · trustc <slug> · …`), for the cut transcript.
    pub compiler_line: String,
    /// `<out_dir>/aterm.dSYM`, present iff dsymutil produced a non-empty DWARF whose UUIDs match the binary.
    pub dsym: Option<PathBuf>,
    /// `<out_dir>/aterm-<ver>-dSYM.zip`: the owner's symbolication archive, never published.
    pub dsym_zip: Option<PathBuf>,
    /// `aterm-<ver>-x86_64-probe.txt`.
    pub x86_64_probe: PathBuf,
}

/// One architecture slice's build, as [`run`] measured it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceBuild {
    pub triple: &'static str,
    pub secs: u64,
    /// Sampled peak physical footprint of the build's descendant tree; unread stays `None`.
    pub peak_bytes: Option<u64>,
    /// Sampled descendant CPU nanoseconds, a lower bound over observed incarnations.
    pub cpu_ns: Option<u64>,
    /// Sampled wait for a core, using the verifier's accounting clock.
    pub starved_ns: Option<u64>,
    /// The units Cargo reported and what the build wrote ([`Units`]); `None` when its messages
    /// could not be read.
    pub units: Option<Units>,
}

/// What one compile call's Cargo messages said (`--message-format=json-render-diagnostics`,
/// `docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1), read as a verify run reads its
/// calls' ([`aterm_verify::compile_account::Artifacts::book`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Units {
    /// `compiler-artifact` messages Cargo found `fresh`.
    pub fresh: u64,
    /// `compiler-artifact` messages it built (`fresh: false`).
    pub compiled: u64,
    /// `compiler-artifact` messages that said neither (no boolean `fresh`, or fields repeated),
    /// counted as neither.
    pub unknown: u64,
    /// The bytes of the regular files the units not reported fresh named, each path and file
    /// once ([`aterm_verify::compile_account::output_bytes`]): what this call wrote, short of the
    /// compiler's intermediates. A reported file that is gone, a directory and a symlink count
    /// nothing.
    pub output_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct BuildUsage {
    peak_bytes: Option<u64>,
    cpu_ns: Option<u64>,
    starved_ns: Option<u64>,
    units: Option<Units>,
}

/// The file name of a universal cut's x86_64 execution record.
#[must_use]
pub fn x86_64_probe_asset_name(version: &str) -> String {
    format!("aterm-{version}-x86_64-probe.txt")
}

/// The x86_64 execution record's text.
#[must_use]
pub fn x86_64_probe_transcript(version: &str, report: &str) -> String {
    let mut text = format!(
        "# aterm {version}: the universal binary's x86_64 slice, RUN under Rosetta by the\n\
         # release cut before it shipped: `arch -x86_64 aterm --diagnose` (cleared\n\
         # environment, a private scratch HOME, SHELL={X86_64_PROBE_SHELL}), whose updater\n\
         # pin and app version were checked against the release's, then one headless\n\
         # session driven over the control socket: `{SLICE_TURN_TEXT}` typed, `x86_64`\n\
         # answered on the screen. The report follows verbatim.\n\
         # docs/DESIGN-intel-just-works-2026-09-14.md section 3.1\n\n"
    );
    text.push_str(report);
    if !report.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[cfg(unix)]
pub(crate) fn current_release_uid() -> Result<u32, String> {
    Ok(crate::trust_lane::proc_tree::euid())
}

#[cfg(not(unix))]
pub(crate) fn current_release_uid() -> Result<u32, String> {
    Err("release target privacy requires Unix ownership and mode semantics".into())
}

/// Open one release-owned directory without accepting a symlink substitution.
#[cfg(unix)]
pub(crate) fn open_release_directory(
    path: &Path,
    expected_uid: u32,
    private: bool,
    what: &str,
) -> Result<File, String> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let before = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {what} {}: {error}", path.display()))?;
    if !before.file_type().is_dir() {
        return Err(format!(
            "{what} is not a real directory: {}",
            path.display()
        ));
    }
    if before.uid() != expected_uid {
        return Err(format!(
            "{what} is owned by uid {}, expected {expected_uid}: {}",
            before.uid(),
            path.display()
        ));
    }
    let directory = atpkg::platform::open_for_sync(path)
        .map_err(|error| format!("open {what} {}: {error}", path.display()))?;
    let opened = directory
        .metadata()
        .map_err(|error| format!("inspect opened {what} {}: {error}", path.display()))?;
    if !opened.file_type().is_dir()
        || opened.uid() != expected_uid
        || (opened.dev(), opened.ino()) != (before.dev(), before.ino())
    {
        return Err(format!(
            "{what} changed while it was opened: {}",
            path.display()
        ));
    }
    if !private && opened.mode() & 0o022 != 0 {
        return Err(format!(
            "{what} is group/other-writable (mode {:03o}): {}",
            opened.mode() & 0o777,
            path.display()
        ));
    }
    if private && opened.mode() & 0o7777 != 0o700 {
        let mut permissions = opened.permissions();
        permissions.set_mode(0o700);
        directory
            .set_permissions(permissions)
            .map_err(|error| format!("make {what} private {}: {error}", path.display()))?;
    }
    let published = std::fs::symlink_metadata(path)
        .map_err(|error| format!("reinspect {what} {}: {error}", path.display()))?;
    if !published.file_type().is_dir()
        || published.uid() != expected_uid
        || (published.dev(), published.ino()) != (opened.dev(), opened.ino())
        || private && published.mode() & 0o777 != 0o700
        || !private && published.mode() & 0o022 != 0
    {
        return Err(format!(
            "{what} is not the opened owned directory: {}",
            path.display()
        ));
    }
    Ok(directory)
}

#[cfg(not(unix))]
pub(crate) fn open_release_directory(
    _path: &Path,
    _expected_uid: u32,
    _private: bool,
    _what: &str,
) -> Result<File, String> {
    Err("release target privacy requires Unix ownership and mode semantics".into())
}

const RELEASE_SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

fn release_tool_path(tool_dir: &Path) -> Result<OsString, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        if tool_dir.as_os_str().as_bytes().contains(&b':') {
            return Err(format!(
                "release tool directory contains a PATH separator: {}",
                tool_dir.display()
            ));
        }
    }
    let mut path = tool_dir.as_os_str().to_owned();
    path.push(":");
    path.push(RELEASE_SYSTEM_PATH);
    Ok(path)
}

/// The directory of the rustup the `rustup` row of `aterm_uds::host::TOOLS` resolves (the
/// stamped copy `verify provision` lays beside its cargo and rustc proxies), held to what a
/// release build runs from it ([`resolve_release_rustup_shim_dir`]).
pub(crate) fn release_rustup_shim_dir() -> Result<PathBuf, String> {
    let rustup = aterm_uds::host::tool("rustup")
        .locate()
        .map_err(|absent| absent.to_string())?;
    release_rustup_shim_dir_of(&rustup)
}

/// [`resolve_release_rustup_shim_dir`] of `rustup` for this process's user.
pub(crate) fn release_rustup_shim_dir_of(rustup: &Path) -> Result<PathBuf, String> {
    resolve_release_rustup_shim_dir(rustup, current_release_uid()?)
}

/// `rustup`'s own directory, canonical, when it is a real directory of `expected_uid`'s that
/// no group or other may write and its `cargo` and `rustup` are real regular executables of
/// that user's: no other directory answers.
fn resolve_release_rustup_shim_dir(rustup: &Path, expected_uid: u32) -> Result<PathBuf, String> {
    let dir = rustup
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| format!("rustup at {} names no directory", rustup.display()))?;
    validate_release_rustup_shim_dir(dir, expected_uid)
}

#[cfg(unix)]
fn validate_release_rustup_shim_dir(
    candidate: &Path,
    expected_uid: u32,
) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt as _;

    let canonical = candidate.canonicalize().map_err(|error| {
        format!(
            "canonicalize rustup shim directory {}: {error}",
            candidate.display()
        )
    })?;
    let directory = open_release_directory(
        &canonical,
        expected_uid,
        false,
        "release rustup shim directory",
    )?;
    let directory_metadata = directory
        .metadata()
        .map_err(|error| format!("inspect release rustup shim directory: {error}"))?;
    for tool in ["cargo", "rustup"] {
        let path = canonical.join(tool);
        let before = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect release {tool} shim {}: {error}", path.display()))?;
        if !before.file_type().is_file() {
            return Err(format!(
                "release {tool} shim is not a real regular file: {}",
                path.display()
            ));
        }
        if before.uid() != expected_uid {
            return Err(format!(
                "release {tool} shim is owned by uid {}, expected {expected_uid}: {}",
                before.uid(),
                path.display()
            ));
        }
        if before.mode() & 0o022 != 0 || before.mode() & 0o111 == 0 {
            return Err(format!(
                "release {tool} shim has unsafe mode {:03o}: {}",
                before.mode() & 0o777,
                path.display()
            ));
        }
        let (_, opened) = crate::durable::open_regular_file_with_meta(&path)
            .map_err(|error| format!("open release {tool} shim {}: {error}", path.display()))?;
        if !opened.file_type().is_file()
            || opened.uid() != expected_uid
            || (opened.dev(), opened.ino()) != (before.dev(), before.ino())
        {
            return Err(format!(
                "release {tool} shim changed while it was opened: {}",
                path.display()
            ));
        }
    }
    let published = std::fs::symlink_metadata(&canonical).map_err(|error| {
        format!(
            "reinspect release rustup shim directory {}: {error}",
            canonical.display()
        )
    })?;
    if !published.file_type().is_dir()
        || published.uid() != expected_uid
        || published.mode() & 0o022 != 0
        || (published.dev(), published.ino())
            != (directory_metadata.dev(), directory_metadata.ino())
    {
        return Err(format!(
            "release rustup shim directory changed during validation: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

#[cfg(not(unix))]
fn validate_release_rustup_shim_dir(
    _candidate: &Path,
    _expected_uid: u32,
) -> Result<PathBuf, String> {
    Err("release rustup shim validation requires Unix ownership and mode semantics".into())
}

fn release_rustup_home(home: &std::ffi::OsStr) -> Result<OsString, String> {
    let value = atpkg::seam::rustup_home_with(
        std::env::var_os("RUSTUP_HOME").as_deref(),
        Some(Path::new(home)),
    )
    .ok_or("release RUSTUP_HOME has no home to default under")?;
    if !value.is_absolute() {
        return Err("release RUSTUP_HOME must be absolute".into());
    }
    Ok(value.into_os_string())
}

fn checked_output(command: &mut Command, what: &str) -> Result<Output, String> {
    let output = command
        .output()
        .map_err(|error| format!("could not {what}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output)
}

/// What a release build reads and writes.
pub(crate) struct Take {
    /// Commit C's tree and its vendored dependencies, sealed and digested.
    pub(crate) snapshot: snapshot::Snapshot,
    /// The workspace's own Cargo config, as C has it (`<src>/.cargo/config.toml`).
    workspace_config: PathBuf,
    /// The warm target root: one `CARGO_TARGET_DIR` per slice under it
    /// ([`Take::slice_target`]), so the slices build at once without queueing on
    /// one directory's cargo lock.
    target_root: PathBuf,
    /// The builds' `CARGO_HOME` root, emptied for each build: one per slice under
    /// it ([`Take::slice_cargo_home`]).
    cargo_home: PathBuf,
    /// `TMPDIR` for every build child.
    temporary_dir: PathBuf,
    /// Lipo, strip and the dSYM, recreated for each build.
    stage: PathBuf,
    release_uid: u32,
    /// `git rev-parse --short=12 C` — pinned as the commit of the binary's identity record and
    /// stamped as the bundle's `ATermGitCommit`, so binary and plist name C alike.
    short_commit: String,
    /// The cache, this take's alone until the take drops (declared last, so it is let
    /// go of last).
    _hold: snapshot::CacheHold,
}

impl Take {
    /// Hold the clone's cache, materialize `commit` of `repo_root` into it and lay the build's directories beside it.
    pub(crate) fn prepare(repo_root: &Path, commit: &str) -> Result<Self, String> {
        let hold = snapshot::CacheRoot::for_repo(repo_root)?.hold()?;
        let cache = hold.cache();
        refuse_held_stage(&cache.stage())?;
        let targo = crate::gates::trust_stage2_bin()
            .map_err(|e| e.to_string())?
            .join("targo");
        let snapshot = snapshot::materialize(repo_root, commit, &hold, &targo)?;
        let workspace_config = snapshot.src.join(".cargo/config.toml");
        let metadata = std::fs::symlink_metadata(&workspace_config)
            .map_err(|error| format!("inspect the workspace Cargo config: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err("the workspace Cargo config is not a regular file".into());
        }
        let release_uid = current_release_uid()?;
        let target_root = cache.target();
        snapshot::private_dir(&target_root, "release target")?;
        let temporary_dir = cache.tmp();
        snapshot::private_dir(&temporary_dir, "release temporary directory")?;
        let cargo_home = cache.build_cargo_home();
        let stage = cache.stage();
        for fresh in [&cargo_home, &stage] {
            match std::fs::remove_dir_all(fresh) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("clear {}: {error}", fresh.display())),
            }
            snapshot::private_dir(fresh, "release build directory")?;
        }
        for slice in [NATIVE_SLICE, X86_64] {
            snapshot::private_dir(&target_root.join(slice), "release slice target")?;
            snapshot::private_dir(&cargo_home.join(slice), "release slice CARGO_HOME")?;
        }
        let short_commit =
            snapshot::git_text(repo_root, &["rev-parse", "--short=12", &snapshot.commit])
                .map_err(|error| format!("abbreviate the release commit: {error}"))?
                .trim()
                .to_owned();
        if short_commit.len() < 12 || !snapshot.commit.starts_with(&short_commit) {
            return Err(format!(
                "git abbreviated {} as {short_commit:?}",
                snapshot.commit
            ));
        }
        Ok(Self {
            snapshot,
            workspace_config,
            target_root,
            cargo_home,
            temporary_dir,
            stage,
            release_uid,
            short_commit,
            _hold: hold,
        })
    }
}

impl Take {
    /// The slice's own `CARGO_TARGET_DIR`.
    pub(crate) fn slice_target(&self, target: Option<&str>) -> PathBuf {
        self.target_root.join(target.unwrap_or(NATIVE_SLICE))
    }

    fn slice_cargo_home(&self, target: Option<&str>) -> PathBuf {
        self.cargo_home.join(target.unwrap_or(NATIVE_SLICE))
    }

    /// Whatever the build did, the bytes it read must still be the bytes digested before it.
    pub(crate) fn verify(&self) -> Result<(), String> {
        self.snapshot
            .verify()
            .map_err(|error| match std::fs::remove_dir_all(&self.target_root) {
                Ok(()) => format!("{error}; the build target it fed was removed"),
                Err(clear) => format!(
                    "{error}; additionally, remove the build target it fed ({}): {clear}",
                    self.target_root.display()
                ),
            })
    }
}

/// The stage's shipped binary's directory: its first exec is stamped there ([`run_with_take`]).
fn ship_stage(stage: &Path) -> PathBuf {
    stage.join("universal").join("ship")
}

/// Refuse to clear `stage` while the shipped binary an earlier build started there never
/// showed it ran: macOS may still be assessing its first exec ([`crate::gates::first_exec_hold`]).
fn refuse_held_stage(stage: &Path) -> Result<(), String> {
    let ship = ship_stage(stage);
    match crate::gates::first_exec_hold(&ship) {
        None => Ok(()),
        Some(left) => Err(format!(
            "{} holds the binary an earlier build started that never showed it ran, so macOS \
             may still be assessing its first exec; it stays — build again in {} s (or after a \
             restart)",
            ship.display(),
            left.as_secs()
        )),
    }
}

/// Run the full release build (snapshot, dual-slice build, lipo, dsymutil, strip, Rosetta check, bundle layout) into `out_dir`.
pub fn run(plan: &BuildPlan) -> Result<BuildOutput, String> {
    let take = Take::prepare(&plan.repo_root, &plan.commit)?;
    let staged_out = take.stage.join("dist");
    let built = run_with_take(plan, &take, &staged_out);
    match (built, take.verify()) {
        (Ok(output), Ok(())) => publish_verified_output(output, &plan.out_dir, take.release_uid),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(build), Err(verify)) => Err(format!("{build}; additionally, {verify}")),
    }
}

fn publish_verified_output(
    mut output: BuildOutput,
    out_dir: &Path,
    expected_uid: u32,
) -> Result<BuildOutput, String> {
    match std::fs::create_dir(out_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "create release output directory {}: {error}",
                out_dir.display()
            ));
        }
    }
    drop(open_release_directory(
        out_dir,
        expected_uid,
        true,
        "release output directory",
    )?);
    output.app = publish_staged_dir(&output.app, out_dir, "unsigned release bundle")?;
    if let Some(staged) = output.dsym.take() {
        output.dsym = Some(publish_staged_dir(&staged, out_dir, "verified dSYM")?);
    }
    if let Some(staged) = output.dsym_zip.take() {
        output.dsym_zip = Some(publish_staged_file(&staged, out_dir, "verified dSYM zip")?);
    }
    output.x86_64_probe =
        publish_staged_file(&output.x86_64_probe, out_dir, "x86_64 slice record")?;
    Ok(output)
}

fn publish_staged_dir(staged: &Path, out_dir: &Path, what: &str) -> Result<PathBuf, String> {
    let name = staged
        .file_name()
        .ok_or_else(|| format!("staged {what} has no file name"))?;
    let published = out_dir.join(name);
    match std::fs::remove_dir_all(&published) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("replace {}: {error}", published.display())),
    }
    std::fs::rename(staged, &published).map_err(|error| {
        format!(
            "publish {what} {} -> {}: {error}",
            staged.display(),
            published.display()
        )
    })?;
    Ok(published)
}

fn publish_staged_file(staged: &Path, out_dir: &Path, what: &str) -> Result<PathBuf, String> {
    let name = staged
        .file_name()
        .ok_or_else(|| format!("staged {what} has no file name"))?;
    let published = out_dir.join(name);
    match std::fs::remove_file(&published) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("replace {}: {error}", published.display())),
    }
    std::fs::rename(staged, &published).map_err(|error| {
        format!(
            "publish {what} {} -> {}: {error}",
            staged.display(),
            published.display()
        )
    })?;
    Ok(published)
}

/// The pool both slices draw their jobs from ([`slice_pool`]).
struct SlicePool {
    pool: aterm_capacity::Jobserver,
    /// A pool this build made, with the Release core claim of all cores it bounds and its
    /// private directory; `None` for the release's own pool. Dropped after `pool`, which removes
    /// its FIFO first.
    own: Option<(aterm_tempfile::TempDir, aterm_capacity::Claim)>,
    /// How long the build's own claim waited for its cores (none for the release's).
    waited: std::time::Duration,
}

/// The slices' pool: the one the release handed this build ([`aterm_capacity::POOL_ENV`]), which
/// the release's own Release core claim already bounds, so the build makes no core claim of its
/// own (a second claim of every core would count the release's other branches' claims and wait
/// for them, serializing the release behind its own build); or, with no pool handed (a `ship
/// build` run by hand), the pool `own` makes under this build's claim. A pool named but not
/// adoptable refuses the build rather than fall back to a claim that would overrun the
/// release's grant.
fn slice_pool(
    inherited: Option<std::io::Result<aterm_capacity::Jobserver>>,
    own: impl FnOnce() -> Result<SlicePool, String>,
) -> Result<SlicePool, String> {
    match inherited {
        Some(Ok(pool)) => Ok(SlicePool {
            pool,
            own: None,
            waited: std::time::Duration::ZERO,
        }),
        Some(Err(error)) => Err(format!(
            "release pool ({}): {error}",
            aterm_capacity::POOL_ENV
        )),
        None => own(),
    }
}

/// A pool of this build's own: a Release core claim of all cores, and a jobserver of the cores it
/// granted in a private directory under `temporary`.
fn own_slice_pool(temporary: &Path) -> Result<SlicePool, String> {
    let claim = aterm_capacity::admit(
        aterm_capacity::Need::new("release slice core pool")
            .rank(aterm_capacity::Rank::Release)
            .cores(aterm_capacity::cores()),
        aterm_capacity::Admit::Wait,
    )
    .map_err(|e| e.to_string())?;
    let dir = aterm_tempfile::Builder::new()
        .prefix("release-pool-")
        .suffix(".noindex")
        .tempdir_in(temporary)
        .map_err(|e| format!("release pool directory: {e}"))?;
    let pool = aterm_capacity::Jobserver::new(&dir.path().join("jobs"), claim.cores())
        .map_err(|e| format!("release jobserver: {e}"))?;
    Ok(SlicePool {
        pool,
        waited: claim.waited(),
        own: Some((dir, claim)),
    })
}

/// Run both slice drivers in their measured order while `parent` remains borrowed and therefore
/// held. The returned native result always precedes the compat result.
fn serial_slices_while<Parent, Slice, Result>(
    _parent: &Parent,
    [native, compat]: [Slice; 2],
    mut run: impl FnMut(Slice) -> Result,
) -> [Result; 2] {
    [run(native), run(compat)]
}

/// Only a continuously normal, unclaimed memory shortage earns the serial escape.
/// A minute is a patience bound, not evidence that the OS load will ever end.
const SLICE_MEMORY_PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

fn serial_escape(short: &aterm_capacity::Shortfall, reading: &aterm_capacity::Reading) -> bool {
    reading.pressure == Some(aterm_sysprobe::MemoryLevel::Normal)
        && short.holders.is_empty()
        && !short.shorts.is_empty()
        && short.shorts.iter().all(|short| {
            matches!(
                short,
                aterm_capacity::Short::Memory {
                    reserved: 0,
                    passes: None,
                    ..
                }
            )
        })
}

/// What one dry look at the slice-memory admission says qualify would do in the same machine
/// state. A normal, unclaimed shortage which also misses the serial envelope takes the exclusive
/// parent: the live admission's two patience bounds change no part of that state, and no
/// claim's end gives the memory back, so the build holds the machine's memory alone as it did
/// when it was first measured. Claimed or pressured shortages remain waits, as they do in the
/// live admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliceMemoryOutcome {
    Shared,
    Serial,
    Exclusive,
    Wait,
}

/// Qualify's slice-memory outcome and its complete person-facing diagnosis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SliceMemoryForecast {
    pub(crate) outcome: SliceMemoryOutcome,
    pub(crate) line: String,
}

enum ParentLook {
    Admitted(Option<aterm_capacity::Reading>),
    Waiting(Option<aterm_capacity::Reading>, aterm_capacity::Shortfall),
}

/// Keep the exact admission sample for the patience clock. A separate earlier read
/// can repeatedly see normal pressure while admission sees warning pressure.
struct SliceMemorySample<'a> {
    machine: &'a dyn aterm_capacity::Machine,
    normal_required: bool,
    reading: std::cell::RefCell<Option<aterm_capacity::Reading>>,
}

impl<'a> SliceMemorySample<'a> {
    fn new(machine: &'a dyn aterm_capacity::Machine, normal_required: bool) -> Self {
        Self {
            machine,
            normal_required,
            reading: std::cell::RefCell::new(None),
        }
    }
}

impl aterm_capacity::Machine for SliceMemorySample<'_> {
    fn read(&self, path: Option<&Path>) -> aterm_capacity::Reading {
        let mut reading = self.machine.read(path);
        *self.reading.borrow_mut() = Some(reading.clone());
        if self.normal_required && reading.pressure != Some(aterm_sysprobe::MemoryLevel::Normal) {
            reading.available = Some(0);
        }
        reading
    }

    fn procs(&self) -> Option<aterm_capacity::Procs> {
        self.machine.procs()
    }

    fn lineage(&self, pid: u32) -> Option<aterm_capacity::Procs> {
        self.machine.lineage(pid)
    }

    fn waits(&self) -> Vec<aterm_toolchain::waits::Waiting> {
        self.machine.waits()
    }
}

fn available_words(reading: Option<&aterm_capacity::Reading>) -> String {
    reading.and_then(|reading| reading.available).map_or_else(
        || "available memory unread".to_owned(),
        |bytes| format!("{} available", aterm_capacity::size(bytes)),
    )
}

/// Poll one of qualify's actual parent needs once. The temporary queue row is necessary for the
/// same ledger turn, claim ordering, process credit and passing rules as live admission; every
/// returned waiter or claim is dropped before another plan is sampled.
fn look_at_slice_parent(
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    need: aterm_capacity::Need,
    normal_required: bool,
) -> Result<ParentLook, String> {
    use aterm_capacity::{Polled, Waiter};
    let sample = SliceMemorySample::new(machine, normal_required);
    let polled = Waiter::enter(ledger, machine, need)
        .map_err(|error| error.to_string())?
        .poll(&sample);
    let reading = sample.reading.into_inner();
    match polled {
        Polled::Admitted(claim) => {
            drop(claim);
            Ok(ParentLook::Admitted(reading))
        }
        Polled::Waiting(waiter, short) => {
            drop(waiter);
            Ok(ParentLook::Waiting(reading, short))
        }
        Polled::Gone(error) => Err(error.to_string()),
        Polled::Unqualified(issue) => Err(format!("slice memory unqualified: {issue:?}")),
    }
}

/// Forecast the shared and serial parents using the same admission decisions as qualify, without
/// sleeping through either patience bound. This seam takes explicit needs so a fixed machine can
/// prove the classification independently of the account's retained compiler book.
pub(crate) fn slice_memory_forecast_for_needs_in(
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    shared: aterm_capacity::Need,
    serial: aterm_capacity::Need,
) -> Result<SliceMemoryForecast, String> {
    let shared_bytes = shared.mem;
    let serial_bytes = serial.mem;
    match look_at_slice_parent(ledger, machine, shared, false)? {
        ParentLook::Admitted(reading) => Ok(SliceMemoryForecast {
            outcome: SliceMemoryOutcome::Shared,
            line: format!(
                "qualify slice memory: shared admits now — {} parent, {}",
                aterm_capacity::size(shared_bytes),
                available_words(reading.as_ref())
            ),
        }),
        ParentLook::Waiting(reading, shared_short) => {
            let shared_can_degrade = reading
                .as_ref()
                .is_some_and(|reading| serial_escape(&shared_short, reading));
            if !shared_can_degrade {
                return Ok(SliceMemoryForecast {
                    outcome: SliceMemoryOutcome::Wait,
                    line: format!(
                        "qualify slice memory: shared {} parent would wait ({shared_short})",
                        aterm_capacity::size(shared_bytes)
                    ),
                });
            }
            match look_at_slice_parent(ledger, machine, serial, true)? {
                ParentLook::Admitted(reading) => Ok(SliceMemoryForecast {
                    outcome: SliceMemoryOutcome::Serial,
                    line: format!(
                        "qualify slice memory: shared {} parent is short ({shared_short}); after 60s it degrades to serial and admits — {} parent, {}",
                        aterm_capacity::size(shared_bytes),
                        aterm_capacity::size(serial_bytes),
                        available_words(reading.as_ref())
                    ),
                }),
                ParentLook::Waiting(reading, serial_short) => {
                    let serial_would_refuse = reading
                        .as_ref()
                        .is_some_and(|reading| serial_escape(&serial_short, reading));
                    Ok(if serial_would_refuse {
                        SliceMemoryForecast {
                            outcome: SliceMemoryOutcome::Exclusive,
                            line: format!(
                                "qualify slice memory: shared {} parent is short ({shared_short}); after 60s it degrades to serial, whose {} parent is short too ({serial_short}); after 60s more the build holds {} alone through both serial slices, with no memory claim beside it",
                                aterm_capacity::size(shared_bytes),
                                aterm_capacity::size(serial_bytes),
                                crate::slice_units::COLD_PARENT
                            ),
                        }
                    } else {
                        SliceMemoryForecast {
                            outcome: SliceMemoryOutcome::Wait,
                            line: format!(
                                "qualify slice memory: shared {} parent is short ({shared_short}); after 60s it degrades to serial, whose {} parent would wait ({serial_short})",
                                aterm_capacity::size(shared_bytes),
                                aterm_capacity::size(serial_bytes)
                            ),
                        }
                    })
                }
            }
        }
    }
}

/// Forecast qualify's slice admission from an already-read compiler-unit book. Complete history
/// uses the exact shared and serial constructors. Incomplete history probes the same continuous
/// exclusive learning parent the two live slices hold; it has no fabricated bounded parent.
pub(crate) fn slice_memory_forecast_in(
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    units: &[crate::slice_units::Units],
) -> Result<SliceMemoryForecast, String> {
    match (
        crate::slice_units::shared_need(units, 2),
        crate::slice_units::serial_need(units),
    ) {
        (Some(shared), Some(serial)) => {
            slice_memory_forecast_for_needs_in(ledger, machine, shared, serial)
        }
        (None, None) => {
            units
                .first()
                .ok_or("no account state directory for release compiler admission")?;
            let need = crate::slice_units::cold_parent_need();
            let bytes = need.mem;
            let what = need.what.clone();
            match look_at_slice_parent(ledger, machine, need, false)? {
                ParentLook::Admitted(reading) => Ok(SliceMemoryForecast {
                    outcome: SliceMemoryOutcome::Exclusive,
                    line: format!(
                        "qualify slice memory: the compiler-unit book is unmeasured, so no shared or serial parent is valid; continuous exclusive learning admits {what}'s {} claim alone now — {}, and holds it through both serial slices",
                        aterm_capacity::size(bytes),
                        available_words(reading.as_ref())
                    ),
                }),
                ParentLook::Waiting(_, short) => Ok(SliceMemoryForecast {
                    outcome: SliceMemoryOutcome::Wait,
                    line: format!(
                        "qualify slice memory: the compiler-unit book is unmeasured, so no shared or serial parent is valid; continuous exclusive learning for {what}'s {} claim would wait ({short})",
                        aterm_capacity::size(bytes)
                    ),
                }),
            }
        }
        _ => Err("inconsistent shared and serial slice memory budgets".to_owned()),
    }
}

/// Read this account's retained compiler-unit book, this user's capacity ledger and the live
/// machine exactly as qualify's admission does, then forecast its first stable outcome.
pub(crate) fn slice_memory_forecast() -> Result<SliceMemoryForecast, String> {
    let ledger = aterm_capacity::Ledger::user().ok_or("no account capacity ledger")?;
    let units = crate::slice_units::Units::live(
        &[ARM64, X86_64],
        2,
        Path::new("unused-dry-run-private-slice-ledger"),
    );
    if units.len() != 2 {
        return Err("no account state directory for release compiler admission".to_owned());
    }
    slice_memory_forecast_in(&ledger, &aterm_capacity::Live, &units)
}

fn admit_slice_parent_in(
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    need: aterm_capacity::Need,
    serial: aterm_capacity::Need,
    mut elapsed: impl FnMut() -> std::time::Duration,
    mut pause: impl FnMut() -> bool,
    mut say: impl FnMut(&str),
) -> Result<(aterm_capacity::Claim, bool), String> {
    use aterm_capacity::{Polled, Waiter};
    let mut degraded = false;
    let mut exclusive = false;
    let mut waiter = Waiter::enter(ledger, machine, need).map_err(|e| e.to_string())?;
    let mut eligible_since = None;
    let mut last_line = None;
    loop {
        let now = elapsed();
        let sample = SliceMemorySample::new(machine, degraded && !exclusive);
        match waiter.poll(&sample) {
            Polled::Admitted(claim) => return Ok((claim, degraded)),
            Polled::Gone(error) => return Err(error.to_string()),
            Polled::Unqualified(issue) => {
                return Err(format!("slice memory unqualified: {issue:?}"));
            }
            Polled::Waiting(next, short) => {
                waiter = next;
                let reading = sample.reading.borrow();
                let pressure_wait = degraded
                    && !exclusive
                    && reading.as_ref().is_some_and(|reading| {
                        reading.pressure != Some(aterm_sysprobe::MemoryLevel::Normal)
                    });
                if last_line.is_none_or(|at| now.saturating_sub(at) >= SLICE_MEMORY_PATIENCE) {
                    if pressure_wait {
                        say(&format!(
                            "capacity: release serial slice memory budget waits ({}s): normal pressure is required for the complete private compiler pool and its driver; admission sample pressure={:?}, available={:?} bytes",
                            now.as_secs(),
                            reading.as_ref().and_then(|r| r.pressure),
                            reading.as_ref().and_then(|r| r.available),
                        ));
                    } else {
                        say(&format!(
                            "capacity: {} waits ({}s): {short}",
                            waiter.record().need.what,
                            now.as_secs()
                        ));
                    }
                    last_line = Some(now);
                }
                // A measured parent this machine can never hold (its memory less what is wired)
                // is not waited for: no patience bound changes that, so the next rung is taken
                // on the same admission sample.
                let beyond = (!exclusive)
                    .then(|| {
                        reading.as_ref().and_then(|reading| {
                            aterm_capacity::never(&waiter.record().need, reading)
                        })
                    })
                    .flatten();
                if beyond.is_some()
                    || (!exclusive
                        && reading
                            .as_ref()
                            .is_some_and(|reading| serial_escape(&short, reading)))
                {
                    let since = *eligible_since.get_or_insert(now);
                    if beyond.is_some() || now.saturating_sub(since) >= SLICE_MEMORY_PATIENCE {
                        drop(reading);
                        let never = beyond.is_some();
                        let why = beyond.map_or_else(
                            || {
                                format!(
                                    "after 60s of normal pressure and unclaimed shortage (total waiting_s={}): {short}",
                                    now.as_secs()
                                )
                            },
                            |why| format!("on this machine ({why})"),
                        );
                        let next = if degraded || serial.mem >= waiter.record().need.mem {
                            // The measured parent cannot be reserved beside this machine's
                            // standing load, and no claim's end will change that. The same
                            // compile ran here under the exclusive parent when it was first
                            // measured, so take that one: it waits only for claims that hold
                            // memory, no memory claim is admitted beside it, and the slices
                            // stay serial.
                            let alone = crate::slice_units::cold_parent_need();
                            say(&format!(
                                "==> release slices: {} cannot be reserved {why}; holding {} alone through both serial slices instead, with no memory claim beside it",
                                waiter.record().need.what,
                                alone.what
                            ));
                            exclusive = true;
                            alone
                        } else {
                            if never {
                                say(&format!(
                                    "==> release slices: {} cannot be reserved {why}: one slice at a time, {} parent",
                                    waiter.record().need.what,
                                    aterm_capacity::size(serial.mem)
                                ));
                            } else {
                                say(&format!(
                                    "==> release slices: degraded after 60s of normal pressure and unclaimed memory shortage: one slice at a time, {} parent; complete private compiler pool plus one driver, compiler claims unchanged, unknown units hold the pool alone; tree waits for both slices",
                                    aterm_capacity::size(serial.mem)
                                ));
                            }
                            serial.clone()
                        };
                        drop(waiter);
                        waiter = Waiter::enter(ledger, machine, next).map_err(|e| e.to_string())?;
                        degraded = true;
                        eligible_since = None;
                        continue;
                    }
                } else {
                    eligible_since = None;
                }
            }
        }
        if !pause() {
            return Err("slice memory admission interrupted".into());
        }
    }
}

fn admit_slice_parent(
    need: aterm_capacity::Need,
    serial: aterm_capacity::Need,
) -> Result<(aterm_capacity::Claim, bool), String> {
    let ledger = aterm_capacity::Ledger::user().ok_or("no account capacity ledger")?;
    let started = Instant::now();
    admit_slice_parent_in(
        &ledger,
        &aterm_capacity::Live,
        need,
        serial,
        || started.elapsed(),
        || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            crate::interrupt::caught().is_none()
        },
        |line| println!("{line}"),
    )
}

fn build_slices(
    plan: &BuildPlan,
    take: &Take,
    logs: &Path,
) -> Result<(Vec<PathBuf>, Vec<SliceBuild>), String> {
    crate::interrupt::install();
    // The x86_64 compat slice is the STOCK EXCEPTION (see the note at the file's head).
    require_rustup_target(X86_64)?;
    let slice_pool = slice_pool(aterm_capacity::Jobserver::inherited(), || {
        own_slice_pool(&take.temporary_dir)
    })?;
    let pool = &slice_pool.pool;
    println!(
        "==> release pool: S={} waiting_s={:.3} ({})",
        pool.slots(),
        slice_pool.waited.as_secs_f64(),
        if slice_pool.own.is_some() {
            "this build's own Release core claim"
        } else {
            "inherited from the release"
        }
    );
    let slices: [(&'static str, Option<&'static str>, &str); 2] = [
        (ARM64, None, "Trust toolchain (+t): native build"),
        (
            X86_64,
            Some(X86_64),
            "upstream stable (+r): --target compat slice",
        ),
    ];
    // Exact or same-metric prior final-link records give both slices one global parent and one
    // private compiler ledger. Otherwise one exclusive parent remains held while the two slices
    // learn serially, with no native-to-compat admission gap.
    let at_once = if matches!(plan.sequencing, Sequencing::OneAtATime) {
        1
    } else {
        2
    };
    let budget_ledger = aterm_tempfile::Builder::new()
        .prefix("release-slice-budget-")
        .suffix(".noindex")
        .tempdir_in(&take.temporary_dir)
        .map_err(|error| format!("release slice budget ledger: {error}"))?;
    let units = crate::slice_units::Units::live(
        &slices.map(|(triple, _, _)| triple),
        at_once,
        budget_ledger.path(),
    );
    if units.len() != slices.len() {
        return Err("no account state directory for release compiler admission".to_owned());
    }
    let mut serial = matches!(plan.sequencing, Sequencing::OneAtATime);
    let shared_need = crate::slice_units::shared_need(&units, at_once);
    let shared = shared_need.is_some();
    let slice_parent = if let Some(need) = shared_need {
        let (claim, degraded) = {
            println!(
                "==> release slices: reserve one {} shared memory budget before either slice",
                aterm_capacity::size(need.mem)
            );
            admit_slice_parent(
                need,
                crate::slice_units::serial_need(&units).ok_or("no serial slice budget")?,
            )?
        };
        serial |= degraded;
        claim
    } else {
        serial = true;
        let need = crate::slice_units::cold_parent_need();
        println!(
            "==> release slices: reserve one {} exclusive cold-learning parent through both serial slices",
            aterm_capacity::size(need.mem)
        );
        aterm_capacity::admit(need, aterm_capacity::Admit::Wait)
            .map_err(|error| error.to_string())?
    };
    let one = |(triple, target, lane): (&'static str, Option<&'static str>, &str),
               admitted: Option<std::sync::mpsc::Sender<()>>| {
        let log = logs.join(format!("{triple}.log"));
        let t = Instant::now();
        println!(
            "==> [{triple}] start {} — {lane} (SOURCE_DATE_EPOCH={}; log {})",
            clock_now(),
            plan.build_number,
            log.display()
        );
        let built = (|| {
            let units = units
                .iter()
                .find(|u| u.triple() == triple)
                .ok_or("no account state directory for release compiler admission")?;
            if shared {
                println!(
                    "==> [{triple}] memory: compiler units spend the shared {} budget",
                    aterm_capacity::size(units.budget_bytes().unwrap_or(0))
                );
            } else {
                println!(
                    "==> [{triple}] memory: compilers learn under the continuously held {}",
                    crate::slice_units::COLD_PARENT
                );
            }
            if let Some(admitted) = admitted {
                let _ = admitted.send(());
            }
            let run = SliceRun {
                verb: "build",
                pkg: "aterm",
                target,
                target_dir: take.slice_target(target),
                build_number: plan.build_number,
                index_hint: plan.index_hint.as_deref(),
                static_crt: false,
                units: Some(units),
            };
            run_slice_profile(take, &run, Some(&log), None, Some(pool), false)
        })();
        let secs = t.elapsed().as_secs();
        match built {
            Ok(usage) => {
                println!(
                    "{}",
                    slice_done_line(triple, &fmt_elapsed(t), usage.peak_bytes)
                );
                Ok(SliceBuild {
                    triple,
                    secs,
                    peak_bytes: usage.peak_bytes,
                    cpu_ns: usage.cpu_ns,
                    starved_ns: usage.starved_ns,
                    units: usage.units,
                })
            }
            Err(error) => Err(format!("[{triple}] {error}\n{}", log_tail(&log, 40))),
        }
    };
    let results: Vec<Result<SliceBuild, String>> = if serial {
        serial_slices_while(&slice_parent, slices, |slice| one(slice, None))
            .into_iter()
            .collect()
    } else {
        std::thread::scope(|scope| {
            // The native slice is the longest measured chain. Admit it before
            // the compat slice asks; the latter still runs beside it when it fits.
            let (admitted, ready) = std::sync::mpsc::channel();
            let native = scope.spawn(move || one(slices[0], Some(admitted)));
            let _ = ready.recv(); // A failed native admission closes the channel too.
            let compat = scope.spawn(move || one(slices[1], None));
            let handles = [native, compat];
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err("a slice build's thread panicked".to_string()))
                })
                .collect()
        })
    };
    // The parent and its private ledger outlive both driver waits and every compiler each driver
    // reaps. The cold parent is one continuous exclusion across the serial handoff.
    drop(slice_parent);
    drop(budget_ledger);
    let mut builds = Vec::new();
    let mut failed = Vec::new();
    for result in results {
        match result {
            Ok(build) => builds.push(build),
            Err(error) => failed.push(error),
        }
    }
    if !failed.is_empty() {
        return Err(failed.join("\n"));
    }
    let bins = vec![
        take.slice_target(None).join("release/aterm"),
        target_bin(&take.slice_target(Some(X86_64)), X86_64, "aterm"),
    ];
    Ok((bins, builds))
}

/// The line a finished slice prints: its triple, its wall time and, where the meter read one,
/// its peak as the meter measured it, the sampled physical footprint of the build's descendant
/// processes ([`SliceBuild::peak_bytes`]), not resident memory.
fn slice_done_line(triple: &str, elapsed: &str, peak_bytes: Option<u64>) -> String {
    format!(
        "==> [{triple}] done in {elapsed}{}",
        peak_bytes.map_or_else(String::new, |b| format!(
            " (peak {} MiB footprint, sampled descendants)",
            b / (1024 * 1024)
        ))
    )
}

fn clock_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let day = secs % 86_400;
    format!("{:02}:{:02}:{:02}Z", day / 3600, day % 3600 / 60, day % 60)
}

fn log_tail(log: &Path, n: usize) -> String {
    let text = crate::durable::read_text(log).unwrap_or_default();
    let mut lines: Vec<&str> = text.lines().rev().take(n).collect();
    lines.reverse();
    let mut out = String::with_capacity(lines.iter().map(|l| l.len() + 7).sum());
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str("    | ");
        out.push_str(line);
    }
    out
}

fn run_with_take(plan: &BuildPlan, take: &Take, symbol_out: &Path) -> Result<BuildOutput, String> {
    snapshot::private_dir(symbol_out, "release symbol stage")?;

    let logs = std::env::var_os(SLICE_LOGS_ENV).map(PathBuf::from);
    let (slices, slice_builds) = build_slices(plan, take, logs.as_deref().unwrap_or(&take.stage))?;
    finish_build(plan, take, symbol_out, slices, slice_builds)
}

/// Finish already-reaped release slices through the production post-link and bundle stages.
fn finish_build(
    plan: &BuildPlan,
    take: &Take,
    symbol_out: &Path,
    slices: Vec<PathBuf>,
    slice_builds: Vec<SliceBuild>,
) -> Result<BuildOutput, String> {
    snapshot::private_dir(symbol_out, "release symbol stage")?;
    // Keep Cargo's linked outputs untouched: a fresh hop copies, stamps and signs those same
    // bytes again. Mutating a target output would invalidate its output-integrity evidence.
    let mut stamped = Vec::new();
    let mut unstamped = Vec::new();
    let slice_store = take._hold.cache().artifacts();
    let identity = aterm_stamp::Stamp {
        app_version: plan.short_version.clone().leak(),
        git_commit: take.short_commit.clone().leak(),
        git_commit_full: take.snapshot.commit.clone().leak(),
        build_number: plan.build_number.to_string().leak(),
        build_time: crate::bundle::epoch_to_rfc3339(plan.build_number).leak(),
        index_hint: plan.index_hint.clone().unwrap_or_default().leak(),
        release_build: true,
        ..aterm_stamp::Stamp::UNSTAMPED
    };
    let stamp = aterm_stamp::encode(&identity)?;
    for (index, slice) in slices.iter().enumerate() {
        let copy = take.stage.join(format!("stamped-{index}"));
        crate::durable::copy_regular_fresh(slice, &copy)
            .map_err(|e| format!("copy stamp slice: {e}"))?;
        // x86 linkers need not emit an ad hoc signature. Establish the signature command
        // before taking U, so both architectures use the same normalization.
        crate::postlink::resign(&copy)?;
        let kept = crate::postlink::keep_unstamped(&copy, &slice_store)?;
        crate::durable::copy_regular_fresh(&kept, &copy)
            .map_err(|e| format!("copy content-addressed slice: {e}"))?;
        unstamped.push(kept);
        let content = crate::postlink::normalized(
            &crate::durable::read_regular_bytes(&copy, u64::MAX).map_err(|e| e.to_string())?,
        )?;
        crate::postlink::stamp(&copy, &stamp)?;
        crate::postlink::resign(&copy)?;
        if crate::postlink::normalized(
            &crate::durable::read_regular_bytes(&copy, u64::MAX).map_err(|e| e.to_string())?,
        )? != content
        {
            return Err("post-link stamping changed product content".into());
        }
        stamped.push(copy);
    }
    crate::postlink::prune_unstamped(&slice_store, &unstamped)?;
    let slices = stamped;

    if let Some(expected) = &plan.expected_update_pin_sha256 {
        verify_built_slice_update_pins(&plan.repo_root, &slices, expected)?;
        println!(
            "    updater pin: every architecture slice embeds {}…",
            &expected[..12]
        );
    }

    let universal = take.stage.join("universal");
    snapshot::private_dir(&universal, "release universal stage")?;
    let fat = universal.join("aterm");
    lipo_or_copy(&slices, &fat)?;
    let archs = lipo_archs(&fat)?;
    println!("    universal binary: {archs}");

    let (dsym, dsym_zip) = extract_dsym(&fat, symbol_out, &plan.short_version, &plan.repo_root)?;

    let ship_dir = ship_stage(&take.stage);
    snapshot::private_dir(&ship_dir, "release ship stage")?;
    let shipped = ship_dir.join("aterm");
    crate::durable::copy_regular_fresh(&fat, &shipped)
        .map_err(|e| format!("copy {}: {e}", fat.display()))?;
    make_executable(&shipped)?;
    let _ = command(aterm_uds::os_path!("strip"))
        .arg("-x")
        .arg(&shipped)
        .current_dir(&plan.repo_root)
        .output();
    crate::postlink::resign(&shipped)?;

    if let Some(expected) = &plan.expected_update_pin_sha256 {
        verify_shipped_update_pin_slices(&shipped, &archs, expected)?;
        println!("    updater pin: every final shipped architecture structurally verified");
    }

    // The copy's first exec, stamped before it: only an exit of its own shows it ran, and a
    // stage without that stays ([`refuse_held_stage`]).
    aterm_uds::deadline::stamp_exec(&ship_dir);
    let probed = command(&shipped)
        .arg("--diagnose")
        .current_dir(&plan.repo_root)
        .output();
    if probed.as_ref().is_ok_and(|out| out.status.code().is_some()) {
        let _ = aterm_uds::deadline::try_mark_ran(&ship_dir);
    }
    let diagnose = match probed {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        _ => {
            return Err(
                "compiler provenance probe failed: the built aterm binary did not answer \
                 --diagnose on this host"
                    .to_string(),
            );
        }
    };
    crate::postlink::identity_report(&identity, &diagnose)?;
    println!("    identity: running native image reports the post-link stamp");
    if let Some(expected) = &plan.expected_update_pin_sha256 {
        validate_slice_update_pin_reports(expected, &[("final universal binary", &diagnose)])?;
        println!("    updater pin: final universal runtime cross-check passed");
    }
    validate_app_version_reports(
        &plan.short_version,
        &[("final universal binary", &diagnose)],
    )?;
    let version_output = command(&shipped)
        .arg("--version")
        .current_dir(&plan.repo_root)
        .output()
        .map_err(|error| format!("execute final universal binary --version: {error}"))?;
    if !version_output.status.success() {
        return Err(format!(
            "app-version probe failed: final universal binary --version exited {}",
            version_output.status
        ));
    }
    validate_cli_app_version(&plan.short_version, &version_output.stdout)?;
    println!(
        "    app version: {} (diagnostics + CLI identity gates passed)",
        plan.short_version
    );
    let compiler_line = diagnose
        .lines()
        .find_map(|l| l.strip_prefix("compiler:"))
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if !compiler_line.starts_with("Trust") {
        return Err(format!(
            "compiler provenance gate: the native slice reports {compiler_line:?} — not a \
             Trust-flavor build. The repo compiles with Trust always; the native lane must \
             have been driven by something other than the stage2 targo/trustc (stale env, \
             wrong TRUST_STAGE2_BIN?). Fix the toolchain resolution and release again"
        ));
    }
    println!("    compiler: {compiler_line}  (Trust provenance gate passed)");
    let objc_line = diagnose
        .lines()
        .find_map(|l| l.strip_prefix("objc:"))
        .map(str::trim)
        .unwrap_or("(no objc: line in --diagnose — binary predates the probe)");
    println!("    objc: {objc_line}");

    let scratch = private_slice_scratch()?;
    let probed = (|| {
        let report = x86_64_slice_runs(&shipped, &scratch.join("home"), &mut |command| {
            command.output()
        })?;
        let label = "x86_64 slice under Rosetta";
        crate::postlink::identity_report(&identity, &report)?;
        if let Some(expected) = &plan.expected_update_pin_sha256 {
            validate_slice_update_pin_reports(expected, &[(label, &report)])?;
        }
        validate_app_version_reports(&plan.short_version, &[(label, &report)])?;
        slice_drives_a_session(
            &shipped,
            SliceArch::X86_64,
            Renderer::Cpu,
            &scratch,
            &mut |command| command.spawn(),
            &mut |command| command.output(),
            SLICE_SOCKET_WAIT,
        )?;
        Ok::<_, String>(report)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    let report = probed?;
    let record = symbol_out.join(x86_64_probe_asset_name(&plan.short_version));
    crate::durable::write_fresh(
        &record,
        x86_64_probe_transcript(&plan.short_version, &report),
    )
    .map_err(|error| format!("write {}: {error}", record.display()))?;
    println!(
        "    x86_64 slice: ran under Rosetta — --diagnose answered, pin and version agree, \
         a headless session answered `{SLICE_TURN_TEXT}` with x86_64 (record: {})",
        x86_64_probe_asset_name(&plan.short_version)
    );

    let app = symbol_out.join("aterm.app");
    crate::bundle::assemble_at(
        &crate::bundle::BundleSpec {
            repo_root: take.snapshot.src.clone(),
            out_dir: symbol_out.to_path_buf(),
            short_version: plan.short_version.clone(),
            build_number: plan.build_number,
            bundle_id: crate::bundle::BUNDLE_ID.to_string(),
            git_commit: take.short_commit.clone(),
            aterm_bin: shipped.clone(),
        },
        &app,
    )?;

    Ok(BuildOutput {
        app,
        snapshot_digest: take.snapshot.digest.clone(),
        slices: slice_builds,
        archs,
        compiler_line,
        dsym,
        dsym_zip,
        x86_64_probe: record,
    })
}

fn cargo_build_args(
    verb: &str,
    workspace_config: &Path,
    source_config: &Path,
    manifest: &Path,
    pkg: &str,
    target: Option<&str>,
    static_crt: bool,
) -> Vec<OsString> {
    let mut args = Vec::with_capacity(18);
    if target.is_none() {
        args.push("--unverified".into());
    }
    args.extend([
        "--config".into(),
        workspace_config.as_os_str().to_owned(),
        "--config".into(),
        source_config.as_os_str().to_owned(),
    ]);
    if let (true, Some(triple)) = (static_crt, target) {
        args.extend([
            "--config".into(),
            aterm_bootstrap_verify::pin::static_build_config(triple).into(),
        ]);
    }
    args.extend([
        verb.into(),
        "--manifest-path".into(),
        manifest.as_os_str().to_owned(),
        "--release".into(),
        "--locked".into(),
        "--offline".into(),
        "-p".into(),
        pkg.into(),
        // Cargo's messages on stdout, for the units the call built and found fresh
        // ([`Units`]); the compiler's diagnostics still read as text in the log.
        "--message-format=json-render-diagnostics".into(),
    ]);
    if verb == "build" && pkg == "aterm" {
        args.extend(["--bin".into(), "aterm".into()]);
    }
    if let Some(triple) = target {
        args.extend(["--target".into(), triple.into()]);
    }
    args
}

fn anatomy_args(mut args: Vec<OsString>, target: Option<&str>) -> Vec<OsString> {
    let timings_key = if target.is_some() {
        "target.x86_64-apple-darwin.rustflags"
    } else {
        "target.'cfg(trust_verify)'.rustflags"
    };
    // Keep Cargo's global --config values in one argument scope. A post-subcommand config
    // shadows the pre-subcommand sealed source configs.
    let at = usize::from(target.is_none());
    args.splice(
        at..at,
        [
            OsString::from("--config"),
            format!("{timings_key}=[\"-Ztime-passes\",\"-Ztime-passes-format=json\"]").into(),
        ],
    );
    args
}

fn pilot_profile_args(
    mut args: Vec<OsString>,
    arm: crate::profile_pilot::Arm,
    target: Option<&str>,
) -> Vec<OsString> {
    if arm == crate::profile_pilot::Arm::FatGui16 {
        let at = usize::from(target.is_none());
        args.splice(
            at..at,
            [
                "--config".into(),
                "profile.release.package.aterm-gui.codegen-units=16".into(),
            ],
        );
    }
    args
}

struct ReleaseEnv<'a> {
    /// The lane's toolchain directory first ([`release_tool_path`]).
    path: OsString,
    home: &'a std::ffi::OsStr,
    tmp: &'a Path,
    cargo_home: PathBuf,
    build_number: u64,
    short_commit: &'a str,
    commit: &'a str,
    target_dir: PathBuf,
    /// The compat slice's rustup home; `None` on the native (Trust) slice.
    rustup_home: Option<OsString>,
    /// [`BuildPlan::index_hint`].
    index_hint: Option<&'a str>,
    /// A release slice's units, this executable (their compilers' wrapper) and the slice's
    /// source root ([`crate::slice_units::Units::env`]); `None` for every other build.
    units: Option<(&'a crate::slice_units::Units, &'a Path, &'a Path)>,
}

fn release_env(e: &ReleaseEnv<'_>) -> Vec<(&'static str, OsString)> {
    use aterm_stamp::input;
    let mut env: Vec<(&'static str, OsString)> = vec![
        ("PATH", e.path.clone()),
        ("HOME", e.home.to_os_string()),
        ("LANG", "C".into()),
        ("LC_ALL", "C".into()),
        ("TMPDIR", e.tmp.as_os_str().to_os_string()),
        ("CARGO_HOME", e.cargo_home.clone().into_os_string()),
        // Preserve the compiler's own reason for each reused or rebuilt unit in slice logs.
        (
            "CARGO_LOG",
            "cargo::core::compiler::fingerprint=info,targo::core::compiler::fingerprint=info"
                .into(),
        ),
        // macOS links a constant placeholder: reservation and index identity are supplied
        // only to the post-link writer. Other platforms still compile their leaf stamp.
        (
            input::MODE,
            if cfg!(target_os = "macos") {
                "postlink"
            } else {
                "commit"
            }
            .into(),
        ),
        (
            input::EPOCH,
            if cfg!(target_os = "macos") {
                "0".into()
            } else {
                e.build_number.to_string().into()
            },
        ),
        ("CARGO_TARGET_DIR", e.target_dir.clone().into_os_string()),
        (
            "CARGO_PROFILE_RELEASE_DEBUG",
            release_profile_debug().0.into(),
        ),
        (
            "CARGO_PROFILE_RELEASE_STRIP",
            release_profile_debug().1.into(),
        ),
        (aterm_capacity::RELEASE_RUN_ENV, "1".into()),
    ];
    if !cfg!(target_os = "macos") {
        env.extend([
            (input::COMMIT, e.short_commit.into()),
            (input::FULL_COMMIT, e.commit.into()),
            (input::DEV_COMMITS, "0".into()),
            (input::RELEASE, "1".into()),
        ]);
        if let Some(hint) = e.index_hint {
            env.push((input::INDEX_HINT, hint.into()));
        }
    }
    if let Some(rustup_home) = &e.rustup_home {
        env.push(("RUSTUP_HOME", rustup_home.clone()));
        env.push(("RUSTUP_TOOLCHAIN", "stable".into()));
    }
    if cfg!(target_os = "linux") {
        env.push(("CARGO_BUILD_JOBS", "4".into()));
    }
    if let Some((units, wrapper, root)) = e.units {
        env.extend(units.env(wrapper, root));
    }
    env
}

const fn release_profile_debug() -> (&'static str, &'static str) {
    if cfg!(target_os = "linux") {
        ("0", "debuginfo")
    } else {
        ("1", "false")
    }
}

/// The pilot's cold build claim. Its target is removed after a successful take, so its disk is
/// scratch: a waiter may wait for the claim's end where those writes could return enough room.
fn pilot_need(arm: crate::profile_pilot::Arm, root: &Path, cores: u32) -> aterm_capacity::Need {
    aterm_capacity::Need::new(format!("profile-pilot {}", arm.name()))
        .mem_gib(
            if matches!(
                arm,
                crate::profile_pilot::Arm::Thin1
                    | crate::profile_pilot::Arm::Thin16
                    | crate::profile_pilot::Arm::Thin16Lines
            ) {
                37
            } else {
                50
            },
        )
        .cores(cores)
        .disk(30 * (1 << 30), root)
        .scratch()
}

struct ContentionSlice {
    report: aterm_json::Value,
    binary: PathBuf,
    build: SliceBuild,
}

pub(crate) struct ContentionBuild {
    slices: Vec<ContentionSlice>,
}

impl ContentionBuild {
    pub(crate) fn reports(&self) -> Vec<aterm_json::Value> {
        self.slices
            .iter()
            .map(|slice| slice.report.clone())
            .collect()
    }
}

/// Resolve a diagnostic build without reserving a release number or reading a signing profile.
/// Real gates use the candidate's commit epoch, shared with their handoff fixtures.
pub(crate) fn contention_plan(
    repo: &Path,
    take: &Take,
    root: &Path,
    full: bool,
) -> Result<BuildPlan, String> {
    if full {
        crate::gates::universal_gate(crate::gates::x86_target_probe, || {
            crate::gates::rosetta_runs(&mut |command| command.output())
        })
        .map_err(|error| error.to_string())?;
        crate::gates::disk_gate(root).map_err(|error| error.to_string())?;
    }
    let manifest = crate::durable::read_text(&take.snapshot.src.join("Cargo.toml"))
        .map_err(|error| error.to_string())?;
    let version = crate::publish::release_version_from_workspace(
        &crate::publish::workspace_version(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let build = if full {
        snapshot::git_text(repo, &["show", "-s", "--format=%ct", &take.snapshot.commit])?
            .trim()
            .parse::<u64>()
            .map_err(|error| format!("candidate build epoch is invalid: {error}"))?
    } else {
        1
    };
    Ok(BuildPlan {
        repo_root: repo.to_owned(),
        commit: take.snapshot.commit.clone(),
        out_dir: root.join("unsigned-build.noindex"),
        build_number: build,
        short_version: version,
        expected_update_pin_sha256: crate::publish::expected_embedded_update_pin(
            aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        )
        .map_err(|error| error.to_string())?,
        sequencing: Sequencing::Parallel,
        index_hint: full.then(index_hint_now).flatten(),
    })
}

/// Finish the measured slices through the production unsigned build, without compiling them
/// again. All post-link identities and probes use the same plan that compiled the slices.
pub(crate) fn finish_contention_build(
    plan: &BuildPlan,
    take: &Take,
    root: &Path,
    built: &ContentionBuild,
    compilers: &crate::artifact::Compilers,
) -> Result<aterm_json::Value, String> {
    let _phase = crate::cut_timing::Timer::parallel_parent()("qualify unsigned build finishing")
        .with_log(Some(&root.join("postlink.log")));
    let started = Instant::now();
    let started_unix_ms = crate::cut_timing::epoch_ms();
    let cpu_before = aterm_sysprobe::jobs::cpu_with_reaped_ns(std::process::id());
    let output = finish_build(
        plan,
        take,
        &take.stage.join("diagnostic-dist"),
        built
            .slices
            .iter()
            .map(|slice| slice.binary.clone())
            .collect(),
        built
            .slices
            .iter()
            .map(|slice| slice.build.clone())
            .collect(),
    )?;
    take.verify()?;
    let output = publish_verified_output(output, &plan.out_dir, take.release_uid)?;
    if &crate::artifact::compilers()? != compilers {
        return Err("compiler or SDK inputs changed during the diagnostic build".into());
    }
    let macho_sha256 = crate::artifact::macho_sha256(&output.app)?;
    let bundle_digest = crate::artifact::bundle_digest(&output.app)?;
    let content_digest = crate::artifact::content_digest(&output.app)?;
    let cpu_s = cpu_before
        .zip(aterm_sysprobe::jobs::cpu_with_reaped_ns(std::process::id()))
        .map(|(before, after)| after.saturating_sub(before) as f64 / 1e9);
    let report = aterm_json::json!({
        "kind":"real-unsigned-build-finishing", "success":true,
        "commit":plan.commit,
        "started_at_unix_ms":started_unix_ms,
        "ended_at_unix_ms":crate::cut_timing::epoch_ms(),
        "wall_s":started.elapsed().as_secs_f64(), "cpu_s":cpu_s,
        "cpu_scope":"build-worker-and-reaped-descendants",
        "build":plan.build_number, "app":output.app, "snapshot_digest":output.snapshot_digest,
        "macho_sha256":macho_sha256, "bundle_digest":bundle_digest, "content_digest":content_digest,
        "trustc":compilers.trustc, "rustc":compilers.rustc, "inputs":compilers.inputs,
        "index_hint":plan.index_hint,
        "architectures":output.archs, "compiler":output.compiler_line,
        "dsym":output.dsym, "dsym_zip":output.dsym_zip,
        "x86_64_probe":output.x86_64_probe,
    });
    crate::durable::write_fresh(&root.join("postlink.result.json"), report.to_string())
        .map_err(|error| error.to_string())?;
    Ok(report)
}

/// Release slices for the contention instrument: the release's drivers, wrapper, memory claims
/// and shared pool. Warm takes use the production targets; cold takes use separate targets.
pub(crate) fn contention_slices(
    plan: &BuildPlan,
    take: &Take,
    root: &Path,
    pool: &aterm_capacity::Jobserver,
    demoted: bool,
    warm: bool,
    ready: Option<std::sync::mpsc::Sender<aterm_json::Value>>,
) -> Result<ContentionBuild, String> {
    require_rustup_target(X86_64)?;
    let budget_ledger = root.join("slice-memory-budget");
    snapshot::private_dir(&budget_ledger, "contention slice memory budget ledger")?;
    let units = crate::slice_units::Units::live(&[ARM64, X86_64], 2, &budget_ledger);
    if units.len() != 2 {
        return Err("no account state directory for release compiler admission".to_owned());
    }
    let memory_started = Instant::now();
    let memory_started_unix_ms = crate::cut_timing::epoch_ms();
    let shared_need = crate::slice_units::shared_need(&units, 2);
    let shared = shared_need.is_some();
    let (slice_parent, serial, cold) = if let Some(need) = shared_need {
        let (claim, serial) = admit_slice_parent(
            need,
            crate::slice_units::serial_need(&units).ok_or("no serial slice budget")?,
        )?;
        (claim, serial, false)
    } else {
        let claim = aterm_capacity::admit(
            crate::slice_units::cold_parent_need(),
            aterm_capacity::Admit::Wait,
        )
        .map_err(|error| error.to_string())?;
        (claim, true, true)
    };
    if let Some(ready) = &ready {
        let record = slice_parent.record();
        ready
            .send(aterm_json::json!({
                "event":"slice-memory-ready",
                "mode":if cold { "cold" } else if serial { "serial" } else { "shared" },
                "label":record.need.what,
                "held":record.held.is_some(),
                "mem":record.need.mem,
                "waiting_s":memory_started.elapsed().as_secs_f64(),
                "started_unix_ms":memory_started_unix_ms,
                "ended_unix_ms":crate::cut_timing::epoch_ms(),
                "claim_waiting_s":slice_parent.waited().as_secs_f64(),
                "claim":{"pid":record.pid,"start_ms":record.start_ms,"seq":record.seq},
            }))
            .map_err(|_| "qualify pilot stopped before slice memory admission".to_owned())?;
    }
    let result = std::thread::scope(|scope| -> Result<Vec<ContentionSlice>, String> {
        let one = |triple: &'static str,
                   target: Option<&'static str>,
                   admitted: Option<std::sync::mpsc::Sender<()>>| {
            let units = units
                .iter()
                .find(|units| units.triple() == triple)
                .ok_or("no account state directory for release compiler admission")?;
            if let Some(admitted) = admitted {
                admitted.send(()).map_err(|_| {
                    "qualify pilot stopped before learning slice admission".to_owned()
                })?;
            }
            let run = SliceRun {
                verb: "build",
                pkg: "aterm",
                target,
                target_dir: if warm {
                    take.slice_target(target)
                } else {
                    root.join(triple).join("build.noindex")
                },
                build_number: plan.build_number,
                index_hint: plan.index_hint.as_deref(),
                static_crt: false,
                units: Some(units),
            };
            let started = Instant::now();
            let usage = run_slice_profile(
                take,
                &run,
                Some(&root.join(format!("{triple}.log"))),
                None,
                Some(pool),
                demoted,
            )?;
            let elapsed = started.elapsed();
            Ok(ContentionSlice {
                report: aterm_json::json!({"triple":triple, "wall_s":elapsed.as_secs_f64(), "cpu_ns":usage.cpu_ns, "peak_bytes":usage.peak_bytes}),
                binary: target.map_or_else(
                    || run.target_dir.join("release/aterm"),
                    |triple| target_bin(&run.target_dir, triple, "aterm"),
                ),
                build: SliceBuild {
                    triple,
                    secs: elapsed.as_secs(),
                    peak_bytes: usage.peak_bytes,
                    cpu_ns: usage.cpu_ns,
                    starved_ns: usage.starved_ns,
                    units: usage.units,
                },
            })
        };
        let join = |handle: std::thread::ScopedJoinHandle<'_, Result<_, String>>| {
            handle
                .join()
                .map_err(|_| "contention slice panicked".to_owned())?
        };
        if serial {
            return serial_slices_while(
                &slice_parent,
                [(ARM64, None), (X86_64, Some(X86_64))],
                |(triple, target)| one(triple, target, None),
            )
            .into_iter()
            .collect();
        }
        if shared {
            // Match the real cut's admission handoff: native announces the already-held shared
            // parent before compat starts. Keeping the same launch edge preserves the private
            // unit ledger's FIFO order in the contention instrument.
            let (native_held, native_admission) = std::sync::mpsc::channel();
            let native = scope.spawn(move || one(ARM64, None, Some(native_held)));
            native_admission
                .recv()
                .map_err(|_| "native shared slice ended before admission".to_owned())?;
            let compat = scope.spawn(move || one(X86_64, Some(X86_64), None));
            return [native, compat].into_iter().map(join).collect();
        }

        // The one cold parent remains held across this explicit native-then-compat sequence.
        serial_slices_while(
            &slice_parent,
            [(ARM64, None), (X86_64, Some(X86_64))],
            |(triple, target)| one(triple, target, None),
        )
        .into_iter()
        .collect()
    });
    // No global reservation can disappear while either driver or compiler is live. The private
    // ledger remains under the take for the qualify sampler; learning workers similarly retain
    // their exclusive guards through their waits.
    drop(slice_parent);
    result.map(|slices| ContentionBuild { slices })
}

/// Build one pilot arm with the same sealed source and environment as a cut.
/// Each target is new: a pilot never inherits Cargo intermediates from a cut.
pub(crate) fn pilot_build(
    take: &Take,
    trial: crate::profile_pilot::Trial,
    root: &Path,
    target: Option<&str>,
    build_number: u64,
    index_hint: Option<&str>,
) -> Result<aterm_json::Value, String> {
    let arm = trial.arm;
    snapshot::private_dir(root, "profile pilot take")?;
    let target_dir = root.join("build.noindex");
    std::fs::create_dir(&target_dir)
        .map_err(|e| format!("fresh pilot target {}: {e}", target_dir.display()))?;
    let log = root.join("build.log");
    let waiting = Instant::now();
    let mut previous_grant = None;
    let claim = loop {
        let claim = aterm_capacity::admit(
            pilot_need(arm, root, trial.jobs.unwrap_or_else(aterm_capacity::cores)),
            aterm_capacity::Admit::Wait,
        )
        .map_err(|e| e.to_string())?;
        if trial.jobs.is_none_or(|jobs| claim.cores() >= jobs) {
            break claim;
        }
        if previous_grant != Some(claim.cores()) {
            println!(
                "profile-pilot: waiting for fixed {} jobs; granted {}",
                trial.jobs.unwrap_or_default(),
                claim.cores()
            );
            previous_grant = Some(claim.cores());
        }
        drop(claim);
        std::thread::sleep(std::time::Duration::from_secs(1));
    };
    let waiting_s = waiting.elapsed().as_secs_f64();
    let load_before = aterm_sysprobe::load_average();
    let started = Instant::now();
    let (usage, max_load1) = crate::profile_pilot::with_load(root, || {
        run_slice_profile(
            take,
            &SliceRun {
                verb: "build",
                pkg: "aterm",
                target,
                target_dir: target_dir.clone(),
                build_number,
                index_hint,
                static_crt: false,
                units: None,
            },
            Some(&log),
            Some((trial, claim.cores())),
            None,
            false,
        )
    })?;
    let usage = usage?;
    let wall = started.elapsed().as_secs_f64();
    take.verify()?;
    let binary = match target {
        Some(triple) => target_bin(&target_dir, triple, "aterm"),
        None => target_dir.join("release/aterm"),
    };
    let gui_rlib_to_binary_s = crate::profile_pilot::gui_link_interval(&log, &binary)?;
    // Keep Cargo's unit/metadata dependency timeline readable after retiring the cold graph.
    crate::profile_pilot::retain_timings(&target_dir, root)?;
    let inspection = crate::profile_pilot::inspect(&binary, root).unwrap_or_else(|error| {
        aterm_json::Value::Object(aterm_json::Map::from([(
            "error".to_string(),
            aterm_json::Value::String(error),
        )]))
    });
    let (sha256, bytes) = crate::dmg::sha256_and_size(&binary)?;
    let retained = root.join("aterm");
    std::fs::rename(&binary, &retained).map_err(|e| format!("retain pilot artifact: {e}"))?;
    let runtime_app = if target.is_none() {
        let manifest = crate::durable::read_text(&take.snapshot.src.join("Cargo.toml"))
            .map_err(|e| format!("read frozen pilot version: {e}"))?;
        let version = crate::publish::release_version_from_workspace(
            &crate::publish::workspace_version(&manifest).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let identity = aterm_stamp::Stamp {
            app_version: version.leak(),
            git_commit: take.short_commit.clone().leak(),
            git_commit_full: take.snapshot.commit.clone().leak(),
            build_number: build_number.to_string().leak(),
            build_time: crate::bundle::epoch_to_rfc3339(build_number).leak(),
            index_hint: index_hint.unwrap_or_default().to_owned().leak(),
            release_build: true,
            ..aterm_stamp::Stamp::UNSTAMPED
        };
        Some(crate::profile_pilot::runtime_app(
            &take.snapshot.src,
            root,
            &retained,
            build_number,
            &identity,
        )?)
    } else {
        None
    };
    let runtime_app_sha256 = runtime_app
        .as_ref()
        .map(|app| crate::dmg::sha256_file(&app.join("Contents/MacOS/aterm")))
        .transpose()?;
    // Preserve evidence, not eight cold intermediate graphs. Nothing in this
    // experiment executes the image whose intermediate directory is removed.
    std::fs::remove_dir_all(&target_dir).map_err(|e| format!("clear pilot intermediates: {e}"))?;
    Ok(aterm_json::Value::Object(aterm_json::Map::from([
        (
            "arm".to_string(),
            aterm_json::Value::String(arm.name().to_string()),
        ),
        (
            "slice".to_string(),
            aterm_json::Value::String(target.unwrap_or(NATIVE_SLICE).to_string()),
        ),
        ("wall_s".to_string(), aterm_json::Value::from(wall)),
        (
            "cpu_s".to_string(),
            usage.cpu_ns.map_or(aterm_json::Value::Null, |ns| {
                aterm_json::Value::from(ns as f64 / 1e9)
            }),
        ),
        (
            "peak_bytes".to_string(),
            usage.peak_bytes.map_or(aterm_json::Value::Null, |b| {
                aterm_json::Value::Number(b.into())
            }),
        ),
        (
            "accounting".to_string(),
            aterm_json::Value::String("sampled-descendant-processes".to_string()),
        ),
        ("binary".to_string(), aterm_json::json!(retained)),
        ("runtime_app".to_string(), aterm_json::json!(runtime_app)),
        (
            "runtime_app_sha256".to_string(),
            aterm_json::json!(runtime_app_sha256),
        ),
        (
            "binary_bytes".to_string(),
            aterm_json::Value::Number(bytes.into()),
        ),
        ("sha256".to_string(), aterm_json::Value::String(sha256)),
        (
            "granted_cores".to_string(),
            aterm_json::Value::Number((claim.cores() as u64).into()),
        ),
        ("waiting_s".to_string(), aterm_json::Value::from(waiting_s)),
        (
            "gui_rlib_to_binary_s".to_string(),
            aterm_json::Value::from(gui_rlib_to_binary_s),
        ),
        (
            "units".to_string(),
            usage.units.map_or(aterm_json::Value::Null, |u| {
                aterm_json::Value::Object(aterm_json::Map::from([
                    (
                        "fresh".to_string(),
                        aterm_json::Value::Number(u.fresh.into()),
                    ),
                    (
                        "compiled".to_string(),
                        aterm_json::Value::Number(u.compiled.into()),
                    ),
                    (
                        "output_bytes".to_string(),
                        aterm_json::Value::Number(u.output_bytes.into()),
                    ),
                ]))
            }),
        ),
        (
            "starved_s".to_string(),
            aterm_json::json!(usage.starved_ns.map(|ns| ns as f64 / 1e9)),
        ),
        ("inspection".to_string(), inspection),
        ("log".to_string(), aterm_json::json!(log)),
        (
            "cargo_timings".to_string(),
            aterm_json::json!(root.join("cargo-timings/cargo-timing.html")),
        ),
        (
            "resource_samples".to_string(),
            aterm_json::json!(root.join("build-resources.jsonl")),
        ),
        ("load_before".to_string(), aterm_json::json!(load_before)),
        (
            "load_after".to_string(),
            aterm_json::json!(aterm_sysprobe::load_average()),
        ),
        (
            "max_load1".to_string(),
            max_load1.map_or(aterm_json::Value::Null, aterm_json::Value::from),
        ),
    ])))
}

/// Run the existing host-only library/wasm instruments against the sealed source,
/// with the pilot's release overrides reaching the examples they compile.
pub(crate) fn pilot_host_checks(
    take: &Take,
    arm: crate::profile_pilot::Arm,
    root: &Path,
    build_number: u64,
    index_hint: Option<&str>,
) -> Result<(), String> {
    let stage2 = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
    let home = std::env::var_os("HOME").ok_or("profile-pilot requires HOME")?;
    let claim = aterm_capacity::admit(
        aterm_capacity::Need::new("profile-pilot host instruments")
            .mem_gib(26)
            .cores(aterm_capacity::cores()),
        aterm_capacity::Admit::Wait,
    )
    .map_err(|e| e.to_string())?;
    for lane in ["vte-engine", "vte-marks", "wasm"] {
        let args = vec![
            "--unverified".into(),
            "--config".into(),
            take.workspace_config.as_os_str().to_owned(),
            "--config".into(),
            take.snapshot.sealed_config.as_os_str().to_owned(),
            "run".into(),
            "--manifest-path".into(),
            take.snapshot.src.join("Cargo.toml").into_os_string(),
            "--profile".into(),
            "test".into(),
            "--locked".into(),
            "--offline".into(),
            "-p".into(),
            "aterm-verify".into(),
            "--bin".into(),
            "aterm-verify".into(),
            "--".into(),
            "perf".into(),
            lane.into(),
            "--no-trend".into(),
        ];
        let mut cmd = release_build_command(
            &stage2.join("targo"),
            args,
            &ReleaseEnv {
                path: release_tool_path(&stage2)?,
                home: &home,
                tmp: &take.temporary_dir,
                cargo_home: take.slice_cargo_home(None),
                build_number,
                short_commit: &take.short_commit,
                commit: &take.snapshot.commit,
                target_dir: root
                    .parent()
                    .ok_or("pilot checks have no parent")?
                    .join("instruments.noindex"),
                rustup_home: None,
                index_hint,
                units: None,
            },
        );
        cmd.current_dir(&take.snapshot.src)
            .envs(arm.environment())
            .env("CARGO_BUILD_JOBS", claim.cores().to_string());
        if lane == "wasm" {
            cmd.arg("--size-only");
        }
        let file = crate::durable::create_fresh(&root.join(format!("{lane}.log")))
            .map_err(|e| e.to_string())?;
        cmd.stderr(file.try_clone().map_err(|e| e.to_string())?)
            .stdout(file);
        let status = cmd.status().map_err(|e| format!("pilot {lane}: {e}"))?;
        if !status.success() {
            return Err(format!(
                "pilot {lane} failed ({status}); see {}",
                root.display()
            ));
        }
        take.verify()?;
    }
    Ok(())
}

/// The shipped channel's published index run now, as a store that has verified nothing finds it ([`atpkg::net::published_index_run`]), for a release build to compile in.
pub(crate) fn index_hint_now() -> Option<String> {
    println!(
        "==> optional index hint: {}s total deadline across all HEADs; no retries",
        atpkg::net::INDEX_HINT_BOUND.as_secs()
    );
    match atpkg::net::published_index_run() {
        Ok(run) => {
            println!(
                "==> index hint {run}: the shipped channel's published run, compiled in for a \
                 fresh install's first discovery round"
            );
            Some(run.to_string())
        }
        Err(why) => {
            println!("==> no index hint ({why}): a fresh install of this build walks from none");
            None
        }
    }
}

fn release_build_command(driver: &Path, args: Vec<OsString>, env: &ReleaseEnv<'_>) -> Command {
    use aterm_uds::spawnfd::Tether as _;
    let mut cmd = command(driver);
    cmd.current_dir("/")
        .args(args)
        .env_clear()
        .envs(release_env(env))
        .stdin(aterm_uds::spawnfd::Stdio::null());
    // The cutter's SIGKILL cannot run its forwarding thread or destructors. The kernel
    // watcher owns cleanup of Cargo's entire group even when Cargo itself dies first.
    #[cfg(unix)]
    cmd.process_group(0).tether();
    #[cfg(not(unix))]
    cmd.tether();
    cmd
}

fn build_one(
    plan: &BuildPlan,
    take: &Take,
    pkg: &str,
    target: Option<&str>,
    log: Option<&Path>,
) -> Result<BuildUsage, String> {
    let run = SliceRun {
        verb: "build",
        pkg,
        target,
        target_dir: take.slice_target(target),
        build_number: plan.build_number,
        index_hint: plan.index_hint.as_deref(),
        static_crt: false,
        units: None,
    };
    run_slice(take, &run, log)
}

/// One static `cargo build --release --target <triple> -p <pkg>` on upstream stable (the STOCK EXCEPTION: the static bootstrap verifier, whose C library is linked in so it runs with no dynamic loader), in that triple's own target directory, and where its `bin` landed.
/// # Errors
/// No stable std for `triple`, and a build that fails.
pub(crate) fn build_static(
    plan: &BuildPlan,
    take: &Take,
    pkg: &str,
    bin: &str,
    triple: &str,
    log: Option<&Path>,
) -> Result<PathBuf, String> {
    require_rustup_target(triple)?;
    let target_dir = take.slice_target(Some(triple));
    let run = SliceRun {
        verb: "build",
        pkg,
        target: Some(triple),
        target_dir: target_dir.clone(),
        build_number: plan.build_number,
        index_hint: None,
        static_crt: true,
        units: None,
    };
    run_slice(take, &run, log)?;
    Ok(target_bin(&target_dir, triple, bin))
}

/// The stable `cargo check` of the x86_64 compat slice's snapshot of `commit`, its output into `log`.
/// The slice builds with stable (the STOCK EXCEPTION), whose std refuses what Trust's allows (a
/// `const fn` calling a method stable does not make const), so `ship check --compat` runs this to
/// find such a tree without building a bundle.
/// # Errors
/// What [`Take::prepare`] refuses, no stable x86_64 std, and a check that fails (the tail of its log).
pub(crate) fn compat_check(repo_root: &Path, commit: &str, log: &Path) -> Result<String, String> {
    compat_available()?;
    let take = Take::prepare(repo_root, commit)?;
    let target_dir = take.target_root.join(COMPAT_CHECK_SLICE);
    snapshot::private_dir(&target_dir, "release compat check target")?;
    let t = Instant::now();
    let run = SliceRun {
        verb: "check",
        pkg: "aterm",
        target: Some(X86_64),
        target_dir,
        build_number: COMPAT_CHECK_EPOCH,
        index_hint: None,
        static_crt: false,
        units: None,
    };
    run_slice(&take, &run, Some(log)).map_err(|error| format!("{error}\n{}", log_tail(log, 40)))?;
    Ok(format!(
        "stable compiles the x86_64 compat slice of {} (cargo check, {})",
        take.short_commit,
        fmt_elapsed(t)
    ))
}

/// The read-only compat lane's infrastructure prerequisite, distinct from a source failure.
pub(crate) fn compat_available() -> Result<(), String> {
    require_rustup_target(X86_64)
}

struct SliceRun<'a> {
    verb: &'static str,
    pkg: &'a str,
    /// `None` for the native Trust slice, the triple for the stable compat slice.
    target: Option<&'a str>,
    target_dir: PathBuf,
    build_number: u64,
    index_hint: Option<&'a str>,
    /// Link the C library in ([`cargo_build_args`]); only with a `target`.
    static_crt: bool,
    /// A release slice's units, whose compilers start under this executable
    /// ([`crate::slice_units`]); `None` for every other build.
    units: Option<&'a crate::slice_units::Units>,
}

fn compat_budget(cmd: &mut Command, held: Option<&str>, jobs: Option<&str>) -> Result<(), String> {
    if held != Some("1") {
        return Ok(());
    }
    let jobs = jobs
        .ok_or("managed compat check has no CPU grant")?
        .parse::<std::num::NonZeroU32>()
        .map_err(|_| "managed compat check has an invalid CPU grant")?;
    cmd.env("CARGO_BUILD_JOBS", jobs.to_string())
        .env("BUILD_SLOT_HELD", "1");
    Ok(())
}

fn run_slice(take: &Take, run: &SliceRun<'_>, log: Option<&Path>) -> Result<BuildUsage, String> {
    run_slice_profile(take, run, log, None, None, false)
}

fn run_slice_profile(
    take: &Take,
    run: &SliceRun<'_>,
    log: Option<&Path>,
    trial: Option<(crate::profile_pilot::Trial, u32)>,
    pool: Option<&aterm_capacity::Jobserver>,
    demoted: bool,
) -> Result<BuildUsage, String> {
    let &SliceRun {
        verb, pkg, target, ..
    } = run;
    snapshot::refuse_root_cargo_config()?;
    // The build driver, per lane. Native: `targo` from the Trust stage2 tool dir, never a
    // PATH `cargo`. Compat: stable's `cargo` via the rustup shim (STOCK EXCEPTION, see the
    // note at the file's head).
    let home = std::env::var_os("HOME").ok_or("release build requires HOME")?;
    let compat_shim = if target.is_some() {
        Some(release_rustup_shim_dir()?)
    } else {
        None
    };
    let driver: (PathBuf, &'static str) = if let Some(shim) = &compat_shim {
        (shim.join("cargo"), "cargo")
    } else {
        let stage2 = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
        (stage2.join("targo"), "targo")
    };
    let (driver_path, driver_name) = driver;
    let tool_dir = if let Some(shim) = &compat_shim {
        shim
    } else {
        driver_path
            .parent()
            .ok_or("Trust build driver has no parent directory")?
    };
    let path = release_tool_path(tool_dir)?;
    let rustup_home = match target {
        Some(_) => Some(release_rustup_home(&home)?),
        None => None,
    };
    let mut args = cargo_build_args(
        verb,
        &take.workspace_config,
        &take.snapshot.sealed_config,
        &take.snapshot.src.join("Cargo.toml"),
        pkg,
        target,
        run.static_crt,
    );
    if let Some((trial, _)) = trial {
        args = pilot_profile_args(args, trial.arm, target);
    }
    if trial.is_some_and(|(trial, _)| trial.anatomy) {
        args = anatomy_args(args, target);
    }
    // A slice's compilers start under this executable ([`crate::slice_units`]).
    let wrapper = run
        .units
        .map(|_| std::env::current_exe())
        .transpose()
        .map_err(|e| format!("this executable, the slice's compiler wrapper: {e}"))?;
    let (program, prefix) = crate::qualify_pilot::invocation(&driver_path, demoted);
    let mut cmd = release_build_command(
        &program,
        prefix.into_iter().chain(args).collect(),
        &ReleaseEnv {
            path,
            home: &home,
            tmp: &take.temporary_dir,
            cargo_home: take.slice_cargo_home(target),
            build_number: run.build_number,
            short_commit: &take.short_commit,
            commit: &take.snapshot.commit,
            target_dir: run.target_dir.clone(),
            rustup_home,
            index_hint: run.index_hint,
            units: run
                .units
                .zip(wrapper.as_deref())
                .map(|(units, wrapper)| (units, wrapper, take.snapshot.src.as_path())),
        },
    );
    if let Some((trial, cores)) = trial {
        let crate::profile_pilot::Trial { arm, anatomy, .. } = trial;
        cmd.envs(arm.environment())
            .env("CARGO_BUILD_JOBS", cores.to_string())
            .arg("--timings");
        if anatomy {
            cmd.arg("-Zsection-timings");
            if target.is_some() {
                // This enables only rustc's unstable JSON timing diagnostic in the pilot's
                // existing stable compat compiler. A release never gets it; section identity
                // against a regular take decides whether this instrument preserves code.
                cmd.env("RUSTC_BOOTSTRAP", "1");
            }
        }
    }
    // A read-only compat check is also used by Background main sampling. Its enclosing cut
    // already holds Release rank when there is one; the compile alone never claims a cut.
    if verb == "check" {
        cmd.env_remove(aterm_capacity::RELEASE_RUN_ENV);
        compat_budget(
            &mut cmd,
            std::env::var("BUILD_SLOT_HELD").ok().as_deref(),
            std::env::var("CARGO_BUILD_JOBS").ok().as_deref(),
        )?;
    }
    // Cargo's implicit job is charged before it starts, just like its rustc workers.
    let _implicit = match pool {
        Some(pool) => {
            let waiting = Instant::now();
            let token = pool
                .attach_cargo(&mut cmd, &|| crate::interrupt::caught().is_some())
                .map_err(|e| format!("release implicit job: {e}"))?;
            println!(
                "==> [{}] jobserver waiting_s={:.3}",
                target.unwrap_or(ARM64),
                waiting.elapsed().as_secs_f64()
            );
            Some(token)
        }
        None => None,
    };
    let lane = if run.static_crt {
        "upstream stable (+r) static"
    } else if target.is_some() {
        "upstream stable (+r) compat"
    } else {
        "Trust (+t)"
    };

    let banner = format!(
        "==> {driver_name} {verb} --release {}-p {pkg}{}  [{lane}]",
        target.map(|t| format!("--target {t} ")).unwrap_or_default(),
        if verb == "build" && pkg == "aterm" {
            " --bin aterm"
        } else {
            ""
        }
    );
    if let Some(log) = log {
        use std::io::Write as _;
        let mut file = crate::durable::create_fresh(log)
            .map_err(|e| format!("create {}: {e}", log.display()))?;
        writeln!(file, "{banner}").map_err(|e| format!("write {}: {e}", log.display()))?;
        cmd.stderr(file);
    } else {
        println!("{banner}");
    }
    // Cargo's messages go to a file, not a pipe: a daemon the build leaves behind holding
    // its stdout cannot keep the count from ending.
    let retained = retained_messages(log, trial.is_some(), run.units.is_some());
    let messages = retained.clone().unwrap_or_else(|| {
        take.temporary_dir.join(format!(
            "{verb}-{pkg}-{}-{}.messages.jsonl",
            target.unwrap_or("native"),
            std::process::id()
        ))
    });
    let sink = crate::durable::create_fresh(&messages)
        .map_err(|e| format!("create {}: {e}", messages.display()))?;
    cmd.stdout(sink);
    let started = Instant::now();
    let spawned = crate::interrupt::GroupChild::spawn(&mut cmd);
    cmd.stdout(aterm_uds::spawnfd::Stdio::null());
    let child = spawned.map_err(|e| format!("spawn {driver_name} for {pkg}: {e}"))?;
    let (status, mut usage) =
        wait_with_peak(child).map_err(|e| format!("wait for {driver_name} for {pkg}: {e}"))?;
    let wall = started.elapsed();
    usage.units = crate::durable::open_regular_file(&messages)
        .and_then(units_of)
        .inspect_err(|e| println!("==> {pkg}: cannot read Cargo's messages: {e}"))
        .ok();
    if retained.is_none() {
        let _ = std::fs::remove_file(&messages);
    }
    let call = format!("{verb} {pkg} {}", target.unwrap_or("native"));
    let line = compile_line(&call, usage, wall);
    let event = build_cpu_event(&call, pkg, target, verb, status, usage, wall, log);
    println!("{line}\n{event}");
    if let Some(log) = log {
        use std::io::Write as _;
        let mut file = crate::durable::open_append(log, false)
            .map_err(|e| format!("append {}: {e}", log.display()))?;
        writeln!(file, "{line}\n{event}").map_err(|e| format!("append {}: {e}", log.display()))?;
    }
    if !status.success() {
        return Err(format!("{driver_name} {verb} -p {pkg} failed ({status})"));
    }
    Ok(usage)
}

/// Cargo transports wrapper JSON as compiler messages on stdout, not as rendered stderr.
/// Choose its permanent path before spawning, so even a killed driver keeps partial events.
fn retained_messages(log: Option<&Path>, pilot: bool, wrapped: bool) -> Option<PathBuf> {
    log.filter(|_| pilot || wrapped)
        .map(|log| log.with_extension("messages.jsonl"))
}

/// The longest Cargo message line [`units_of`] reads; a longer one is skipped.
const MESSAGE_LINE_LIMIT: usize = 4 * 1024 * 1024;

/// Count the units Cargo's `messages` report ([`Units`]), each line read once and bounded, by
/// the reading a verify run gives its own calls ([`aterm_verify::compile_account`]).
/// # Errors
/// The read's.
pub(crate) fn units_of(mut messages: impl Read) -> std::io::Result<Units> {
    let mut read = aterm_verify::compile_account::Artifacts::default();
    let mut line = Vec::new();
    let mut overflow = false;
    let mut chunk = [0; 16 * 1024];
    let book = |line: &[u8], read: &mut aterm_verify::compile_account::Artifacts| {
        if let Ok(text) = std::str::from_utf8(line) {
            read.book(text);
        }
    };
    loop {
        let n = messages.read(&mut chunk)?;
        if n == 0 {
            if !overflow {
                book(&line, &mut read);
            }
            break;
        }
        let mut rest = &chunk[..n];
        while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
            let (seg, after) = rest.split_at(pos);
            rest = &after[1..];
            if !overflow {
                if line.is_empty() {
                    if seg.len() <= MESSAGE_LINE_LIMIT {
                        book(seg, &mut read);
                    }
                } else if line.len() + seg.len() <= MESSAGE_LINE_LIMIT {
                    line.extend_from_slice(seg);
                    book(&line, &mut read);
                }
            }
            line.clear();
            overflow = false;
        }
        if !rest.is_empty() && !overflow {
            if line.len() + rest.len() <= MESSAGE_LINE_LIMIT {
                line.extend_from_slice(rest);
            } else {
                overflow = true;
            }
        }
    }
    Ok(Units {
        fresh: read.units.fresh,
        compiled: read.units.compiled,
        unknown: read.units.unknown,
        output_bytes: aterm_verify::compile_account::output_bytes(&read.written),
    })
}

/// One compile call's line (`docs/DESIGN-fast-build-release-2026-11-05.md` §4.1, A1): `-` where
/// nothing measured it. Descendant selection separates concurrent slices sharing a group.
fn compile_line(call: &str, usage: BuildUsage, wall: std::time::Duration) -> String {
    let units = usage.units.map_or_else(
        || "- fresh / - compiled".to_owned(),
        |u| {
            let unknown = if u.unknown > 0 {
                format!(" / {} unknown", u.unknown)
            } else {
                String::new()
            };
            format!("{} fresh / {} compiled{unknown}", u.fresh, u.compiled)
        },
    );
    let cpu = usage.cpu_ns.map_or_else(
        || "-".to_owned(),
        |ns| format!("{:.1}", std::time::Duration::from_nanos(ns).as_secs_f64()),
    );
    let peak = usage
        .peak_bytes
        .map_or_else(|| "-".to_owned(), |b| format!("{} MiB", b / (1024 * 1024)));
    let starved = usage
        .starved_ns
        .map_or_else(|| "-".to_owned(), |ns| format!("{:.1}", ns as f64 / 1e9));
    let bytes = usage.units.map_or_else(
        || "-".to_owned(),
        |u| format!("{} MiB", u.output_bytes / (1024 * 1024)),
    );
    format!(
        "compile {call}: {units}, {cpu} CPU-s, {starved} starved-s, {:.1} s wall, peak {peak}, {bytes} written",
        wall.as_secs_f64()
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one record of one call: each argument is a field it carries"
)]
fn build_cpu_event(
    call: &str,
    package: &str,
    target: Option<&str>,
    verb: &str,
    status: std::process::ExitStatus,
    usage: BuildUsage,
    wall: std::time::Duration,
    log: Option<&Path>,
) -> String {
    format!(
        "release-event: {}",
        aterm_json::Value::Object(aterm_json::Map::from([
            ("schema".to_string(), aterm_json::Value::Number(1u64.into())),
            (
                "event".to_string(),
                aterm_json::Value::String("build-child-finished".to_string()),
            ),
            (
                "call".to_string(),
                aterm_json::Value::String(call.to_string())
            ),
            (
                "package".to_string(),
                aterm_json::Value::String(package.to_string()),
            ),
            (
                "target".to_string(),
                target.map_or(aterm_json::Value::Null, |t| {
                    aterm_json::Value::String(t.to_string())
                }),
            ),
            (
                "verb".to_string(),
                aterm_json::Value::String(verb.to_string())
            ),
            (
                "log".to_string(),
                log.map_or(aterm_json::Value::Null, |path| {
                    aterm_json::Value::String(path.to_string_lossy().into_owned())
                }),
            ),
            (
                "units".to_string(),
                usage.units.map_or(aterm_json::Value::Null, |u| {
                    aterm_json::Value::Object(aterm_json::Map::from([
                        (
                            "fresh".to_string(),
                            aterm_json::Value::Number(u.fresh.into()),
                        ),
                        (
                            "compiled".to_string(),
                            aterm_json::Value::Number(u.compiled.into()),
                        ),
                        (
                            "unknown".to_string(),
                            aterm_json::Value::Number(u.unknown.into()),
                        ),
                    ]))
                }),
            ),
            (
                "output_bytes".to_string(),
                usage.units.map_or(aterm_json::Value::Null, |u| {
                    aterm_json::Value::Number(u.output_bytes.into())
                }),
            ),
            (
                "outcome".to_string(),
                aterm_json::Value::String(if status.success() { "ok" } else { "FAIL" }.to_string(),),
            ),
            (
                "exit_code".to_string(),
                status
                    .code()
                    .map_or(aterm_json::Value::Null, |c| aterm_json::Value::Number(
                        (c as i64).into()
                    )),
            ),
            (
                "execution_s".to_string(),
                aterm_json::Value::from(wall.as_secs_f64()),
            ),
            (
                "child_cpu".to_string(),
                usage.cpu_ns.map_or(aterm_json::Value::Null, |ns| {
                    aterm_json::Value::Object(aterm_json::Map::from([
                        (
                            "scope".to_string(),
                            aterm_json::Value::String("sampled-descendant-processes".to_string()),
                        ),
                        (
                            "observed_s".to_string(),
                            aterm_json::Value::from(
                                std::time::Duration::from_nanos(ns).as_secs_f64(),
                            ),
                        ),
                    ]))
                }),
            ),
            (
                "starved_s".to_string(),
                aterm_json::json!(usage.starved_ns.map(|ns| ns as f64 / 1e9)),
            ),
            (
                "peak_bytes".to_string(),
                usage
                    .peak_bytes
                    .map_or(aterm_json::Value::Null, |b| aterm_json::Value::Number(
                        b.into()
                    )),
            ),
        ]))
    )
}

/// Wait for `child` while the verifier's clock samples its descendants, including those in
/// other process groups. This changes no signal custody or memory admission.
fn wait_with_peak(
    mut child: crate::interrupt::GroupChild,
) -> std::io::Result<(std::process::ExitStatus, BuildUsage)> {
    let meter = aterm_verify::exec::UsageMeter::start(child.id());
    let forwarding = crate::interrupt::Running::enter(child.id());
    child.drain()?;
    // Stop forwarding while the unreaped leader still pins its process-group identity.
    drop(forwarding);
    let status = child.wait()?;
    let usage = meter.finish();
    Ok((
        status,
        BuildUsage {
            peak_bytes: usage.map(|u| u.peak_bytes),
            cpu_ns: usage.and_then(|u| u64::try_from(u.cpu.as_nanos()).ok()),
            starved_ns: usage.and_then(|u| u64::try_from(u.starved.as_nanos()).ok()),
            units: None,
        },
    ))
}

fn require_rustup_target(triple: &str) -> Result<(), String> {
    let home = std::env::var_os("HOME").ok_or("compat target probe requires HOME")?;
    let rustup_home = release_rustup_home(&home)?;
    let shim = release_rustup_shim_dir()?;
    let rustup = shim.join("rustup");
    let out = command(&rustup)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", release_tool_path(&shim)?)
        .env("RUSTUP_HOME", rustup_home)
        .env("RUSTUP_TOOLCHAIN", "stable")
        .args(["target", "list", "--installed"])
        .current_dir("/")
        .output()
        .map_err(|e| format!("rustup not runnable ({e}) — install rustup"))?;
    if !out.status.success() {
        return Err(format!(
            "rustup target list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if !String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim() == triple)
    {
        return Err(format!(
            "rust std for {triple} is not installed on stable — run \
             `targo --unverified verify provision rustup`, which puts the release chain's stable \
             in place with it (the release is universal)"
        ));
    }
    Ok(())
}

/// The whole `-vV` answer of the Trust `trustc` the native slice builds with
/// ([`crate::gates::trust_stage2_bin`]).
/// # Errors
/// No Trust toolchain, or a `trustc` that does not answer `-vV`.
pub(crate) fn trust_rustc_version() -> Result<String, String> {
    let trustc = crate::gates::trust_stage2_bin()
        .map_err(|e| e.to_string())?
        .join("trustc");
    let out = checked_output(
        command(&trustc).arg("-vV"),
        &format!("ask {} -vV", trustc.display()),
    )?;
    String::from_utf8(out.stdout).map_err(|e| format!("{} -vV is not UTF-8: {e}", trustc.display()))
}

/// The whole `-vV` answer of stable's `rustc`: the identity a static build records.
/// # Errors
/// No rustup shim, or a stable `rustc` that does not answer `-vV`.
pub(crate) fn stable_rustc_version() -> Result<String, String> {
    let home = std::env::var_os("HOME").ok_or("the stable compiler probe requires HOME")?;
    let shim = release_rustup_shim_dir()?;
    let out = checked_output(
        command(shim.join("rustc"))
            .env_clear()
            .env("HOME", &home)
            .env("PATH", release_tool_path(&shim)?)
            .env("RUSTUP_HOME", release_rustup_home(&home)?)
            .env("RUSTUP_TOOLCHAIN", "stable")
            .arg("-vV")
            .current_dir("/"),
        "ask stable's rustc -vV",
    )?;
    String::from_utf8(out.stdout).map_err(|e| format!("stable's rustc -vV is not UTF-8: {e}"))
}

/// The `commit-hash` stable's `rustc -vV` answers.
/// # Errors
/// No rustup shim, or a stable `rustc` that does not answer `-vV` with a commit.
pub(crate) fn stable_rustc_commit() -> Result<String, String> {
    commit_hash_of(&stable_rustc_version()?)
        .ok_or_else(|| "stable's rustc -vV names no commit-hash".to_string())
}

/// The `commit-hash:` line of a `rustc -vV` answer.
pub(crate) fn commit_hash_of(vv: &str) -> Option<String> {
    vv.lines()
        .find_map(|l| l.trim().strip_prefix("commit-hash:"))
        .map(str::trim)
        .filter(|h| !h.is_empty() && *h != "unknown")
        .map(str::to_string)
}

const MACH_HEADER_64_LEN: u64 = 32;
const MACH_MAGIC_64: u32 = 0xfeed_facf;
const MH_EXECUTE: u32 = 2;
const LC_SEGMENT: u32 = 0x1;
const LC_SEGMENT_64: u32 = 0x19;
const SEGMENT_COMMAND_64_LEN: u64 = 72;
const SECTION_64_LEN: u64 = 80;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const UPDATE_PIN_SEGMENT: &[u8] = b"__DATA";
const UPDATE_PIN_SECTION: &[u8] = b"__aterm_upin";
const UPDATE_PIN_RECORD_LEN: u64 = 64;

fn canonical_sha256(value: &str) -> bool {
    aterm_codec::hex::is_lower(value, UPDATE_PIN_RECORD_LEN as usize)
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("fixed-width u32 field"))
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("fixed-width u64 field"))
}

fn macho_name_eq(field: &[u8], expected: &[u8]) -> bool {
    field.len() == 16
        && field.get(..expected.len()) == Some(expected)
        && field[expected.len()..].iter().all(|byte| *byte == 0)
}

fn read_exact_macho<R: Read>(
    reader: &mut R,
    bytes: &mut [u8],
    description: &str,
) -> Result<(), String> {
    reader
        .read_exact(bytes)
        .map_err(|error| format!("read Mach-O {description}: {error}"))
}

fn parse_thin_macho_update_pin<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
) -> Result<String, String> {
    if file_len < MACH_HEADER_64_LEN {
        return Err("thin Mach-O is shorter than its 64-bit header".into());
    }

    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("seek Mach-O header: {error}"))?;
    let mut header = [0_u8; MACH_HEADER_64_LEN as usize];
    read_exact_macho(reader, &mut header, "64-bit header")?;
    if le_u32(&header[0..4]) != MACH_MAGIC_64 {
        return Err("updater-pin proof requires a thin little-endian 64-bit Mach-O".into());
    }
    let cpu_type = le_u32(&header[4..8]);
    if !matches!(cpu_type, CPU_TYPE_ARM64 | CPU_TYPE_X86_64) {
        return Err(format!(
            "updater-pin proof found unsupported Mach-O CPU type {cpu_type:#x}"
        ));
    }
    if le_u32(&header[12..16]) != MH_EXECUTE {
        return Err("updater-pin proof requires a Mach-O executable".into());
    }

    let command_count = u64::from(le_u32(&header[16..20]));
    let command_bytes = u64::from(le_u32(&header[20..24]));
    let commands_end = MACH_HEADER_64_LEN
        .checked_add(command_bytes)
        .ok_or_else(|| "Mach-O load-command range overflowed".to_string())?;
    if commands_end > file_len {
        return Err("Mach-O load-command table extends beyond the file".into());
    }
    if command_count > command_bytes / 8 {
        return Err("Mach-O load-command count cannot fit in sizeofcmds".into());
    }

    let mut command_offset = MACH_HEADER_64_LEN;
    let mut record_offset = None;
    for command_index in 0..command_count {
        if command_offset
            .checked_add(8)
            .is_none_or(|end| end > commands_end)
        {
            return Err(format!(
                "Mach-O load command {command_index} has no complete header"
            ));
        }
        reader
            .seek(SeekFrom::Start(command_offset))
            .map_err(|error| format!("seek Mach-O load command {command_index}: {error}"))?;
        let mut load_header = [0_u8; 8];
        read_exact_macho(reader, &mut load_header, "load-command header")?;
        let command = le_u32(&load_header[0..4]);
        let command_size = u64::from(le_u32(&load_header[4..8]));
        if command_size < 8 {
            return Err(format!(
                "Mach-O load command {command_index} has invalid size {command_size}"
            ));
        }
        let command_end = command_offset
            .checked_add(command_size)
            .ok_or_else(|| format!("Mach-O load command {command_index} range overflowed"))?;
        if command_end > commands_end {
            return Err(format!(
                "Mach-O load command {command_index} extends beyond sizeofcmds"
            ));
        }

        if command == LC_SEGMENT {
            return Err("64-bit Mach-O contains a 32-bit LC_SEGMENT command".into());
        }
        if command == LC_SEGMENT_64 {
            if command_size < SEGMENT_COMMAND_64_LEN {
                return Err(format!(
                    "Mach-O LC_SEGMENT_64 command {command_index} is too short"
                ));
            }
            let mut segment = [0_u8; (SEGMENT_COMMAND_64_LEN - 8) as usize];
            read_exact_macho(reader, &mut segment, "LC_SEGMENT_64 body")?;
            let segment_file_offset = le_u64(&segment[32..40]);
            let segment_file_size = le_u64(&segment[40..48]);
            let segment_file_end = segment_file_offset
                .checked_add(segment_file_size)
                .ok_or_else(|| "Mach-O segment file range overflowed".to_string())?;
            if segment_file_end > file_len {
                return Err("Mach-O segment extends beyond the file".into());
            }
            let section_count = u64::from(le_u32(&segment[56..60]));
            let required_size = SEGMENT_COMMAND_64_LEN
                .checked_add(
                    section_count
                        .checked_mul(SECTION_64_LEN)
                        .ok_or_else(|| "Mach-O section table size overflowed".to_string())?,
                )
                .ok_or_else(|| "Mach-O segment-command size overflowed".to_string())?;
            if required_size > command_size {
                return Err(format!(
                    "Mach-O segment command {command_index} cannot contain its {section_count} sections"
                ));
            }

            for section_index in 0..section_count {
                let mut section = [0_u8; SECTION_64_LEN as usize];
                read_exact_macho(reader, &mut section, "section_64 record")?;
                if !macho_name_eq(&section[0..16], UPDATE_PIN_SECTION) {
                    continue;
                }
                if record_offset.is_some() {
                    return Err("Mach-O contains duplicate __aterm_upin sections".into());
                }
                if !macho_name_eq(&segment[0..16], UPDATE_PIN_SEGMENT)
                    || !macho_name_eq(&section[16..32], UPDATE_PIN_SEGMENT)
                {
                    return Err(format!(
                        "Mach-O __aterm_upin section {section_index} is not in __DATA"
                    ));
                }
                let section_size = le_u64(&section[40..48]);
                if section_size != UPDATE_PIN_RECORD_LEN {
                    return Err(format!(
                        "Mach-O __aterm_upin section has length {section_size}, expected {UPDATE_PIN_RECORD_LEN}"
                    ));
                }
                if le_u32(&section[64..68]) & 0xff != 0 {
                    return Err(
                        "Mach-O __aterm_upin section is not file-backed S_REGULAR data".into(),
                    );
                }
                let section_offset = u64::from(le_u32(&section[48..52]));
                let section_end = section_offset
                    .checked_add(section_size)
                    .ok_or_else(|| "Mach-O updater-pin section range overflowed".to_string())?;
                if section_offset < commands_end || section_end > file_len {
                    return Err("Mach-O updater-pin section points outside file-backed data".into());
                }
                if section_offset < segment_file_offset || section_end > segment_file_end {
                    return Err("Mach-O updater-pin section lies outside its segment".into());
                }
                record_offset = Some(section_offset);
            }
        }

        command_offset = command_end;
    }
    if command_offset != commands_end {
        return Err("Mach-O sizeofcmds contains unclaimed trailing bytes".into());
    }

    let record_offset =
        record_offset.ok_or_else(|| "Mach-O is missing __DATA,__aterm_upin".to_string())?;
    reader
        .seek(SeekFrom::Start(record_offset))
        .map_err(|error| format!("seek Mach-O updater-pin record: {error}"))?;
    let mut record = [0_u8; UPDATE_PIN_RECORD_LEN as usize];
    read_exact_macho(reader, &mut record, "updater-pin record")?;
    let observed = std::str::from_utf8(&record)
        .map_err(|_| "Mach-O updater-pin record is not UTF-8".to_string())?;
    if !canonical_sha256(observed) {
        return Err("Mach-O updater-pin record is not canonical lowercase SHA-256".into());
    }
    Ok(observed.to_string())
}

fn read_thin_macho_update_pin(path: &Path) -> Result<String, String> {
    let (mut file, meta) = crate::durable::open_regular_file_with_meta(path)
        .map_err(|error| format!("open architecture slice {}: {error}", path.display()))?;
    parse_thin_macho_update_pin(&mut file, meta.len())
        .map_err(|error| format!("architecture slice {}: {error}", path.display()))
}

fn validate_embedded_update_pin(expected: &str, observed: &str, label: &str) -> Result<(), String> {
    if !canonical_sha256(expected) {
        return Err("expected updater-pin fingerprint is not canonical lowercase SHA-256".into());
    }
    if !canonical_sha256(observed) || observed != expected {
        return Err(format!(
            "architecture slice {label} embedded updater pin {observed:?} differs from the permanent authority"
        ));
    }
    Ok(())
}

const SHIPPED_ARCHITECTURES: [&str; 2] = [LIPO_ARM64, LIPO_X86_64];

/// `lipo -archs`' answer for the shipped binary, held to [`SHIPPED_ARCHITECTURES`] exactly.
pub(crate) fn validate_lipo_architectures(archs: &str) -> Result<Vec<&str>, String> {
    let observed: Vec<&str> = archs.split_whitespace().collect();
    let expected = &SHIPPED_ARCHITECTURES;
    if observed.len() != expected.len()
        || expected
            .iter()
            .any(|required| observed.iter().filter(|arch| *arch == required).count() != 1)
        || observed.iter().any(|arch| !expected.contains(arch))
    {
        return Err(format!(
            "shipped Mach-O architectures {observed:?} differ from required {expected:?}"
        ));
    }
    Ok(observed)
}

fn validate_final_slice_records(
    expected_pin: &str,
    required_architectures: &[&str],
    records: &[(&str, &str)],
) -> Result<(), String> {
    if records.len() != required_architectures.len() {
        return Err(format!(
            "final shipped updater-pin proof supplied {} records for {} architectures",
            records.len(),
            required_architectures.len()
        ));
    }
    for required in required_architectures {
        let matching: Vec<&str> = records
            .iter()
            .filter_map(|(architecture, record)| (*architecture == *required).then_some(*record))
            .collect();
        let [record] = matching.as_slice() else {
            return Err(format!(
                "final shipped architecture {required} has {} updater-pin records; expected exactly one",
                matching.len()
            ));
        };
        validate_embedded_update_pin(expected_pin, record, required)?;
    }
    if records
        .iter()
        .any(|(architecture, _)| !required_architectures.contains(architecture))
    {
        return Err("final shipped updater-pin proof contains an unexpected architecture".into());
    }
    Ok(())
}

struct PrivateSliceDir(PathBuf);

impl PrivateSliceDir {
    fn create() -> Result<Self, String> {
        aterm_tempfile::Builder::new()
            .prefix("aterm-update-pin-proof-")
            .suffix(".noindex")
            .tempdir()
            .map(|dir| Self(dir.keep()))
            .map_err(|error| format!("create private updater-pin proof directory: {error}"))
    }
}

impl Drop for PrivateSliceDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn verify_shipped_update_pin_slices(
    shipped: &Path,
    archs: &str,
    expected_pin: &str,
) -> Result<(), String> {
    let architectures = validate_lipo_architectures(archs)?;
    let mut owned_records = Vec::with_capacity(architectures.len());

    if architectures.len() == 1 {
        owned_records.push((
            architectures[0].to_string(),
            read_thin_macho_update_pin(shipped)?,
        ));
    } else {
        let temp = PrivateSliceDir::create()?;
        for architecture in &architectures {
            let thin = temp.0.join(architecture);
            let mut command = command(aterm_uds::os_path!("lipo"));
            command
                .arg(shipped)
                .args(["-thin", architecture, "-output"])
                .arg(&thin);
            run_quiet(
                command,
                &format!("lipo -thin {architecture} for updater-pin proof"),
            )?;
            owned_records.push((
                (*architecture).to_string(),
                read_thin_macho_update_pin(&thin)?,
            ));
        }
    }

    let records: Vec<(&str, &str)> = owned_records
        .iter()
        .map(|(architecture, record)| (architecture.as_str(), record.as_str()))
        .collect();
    validate_final_slice_records(expected_pin, &SHIPPED_ARCHITECTURES, &records)
}

/// Require every diagnostics report to carry exactly one app-version field equal to the ledger-derived release identity.
pub fn validate_app_version_reports(
    expected: &str,
    reports: &[(&str, &str)],
) -> Result<(), String> {
    for (label, report) in reports {
        let fields: Vec<&str> = report
            .lines()
            .filter_map(|line| line.strip_prefix("version:"))
            .map(str::trim)
            .collect();
        let [field] = fields.as_slice() else {
            return Err(format!(
                "{label} reported {} diagnostics version fields; expected exactly one",
                fields.len()
            ));
        };
        let observed = field.split_once(" (").map(|(version, _)| version);
        if observed != Some(expected) {
            return Err(format!(
                "{label} diagnostics app version {observed:?} differs from claimed {expected:?}"
            ));
        }
    }
    Ok(())
}

/// Require the one-binary CLI identity to be exactly `aterm <claimed>`.
pub fn validate_cli_app_version(expected: &str, stdout: &[u8]) -> Result<(), String> {
    validate_named_cli_app_version("aterm", expected, stdout)
}

/// Require an argv0 alias identity to be exactly `<name> <claimed>` on LINE ONE.
pub fn validate_named_cli_app_version(
    name: &str,
    expected: &str,
    stdout: &[u8],
) -> Result<(), String> {
    let observed =
        std::str::from_utf8(stdout).map_err(|_| format!("{name} --version output is not UTF-8"))?;
    let wanted = format!("{name} {expected}\n");
    let Some(rest) = observed.strip_prefix(wanted.as_str()) else {
        return Err(format!(
            "{name} --version output {observed:?} does not open with {wanted:?}"
        ));
    };
    let rest = rest
        .strip_prefix(aterm_types::identity::ORIGIN_LINE)
        .and_then(|r| r.strip_prefix('\n'))
        .unwrap_or(rest);
    if !rest.is_empty() && !rest.ends_with('\n') {
        return Err(format!(
            "{name} --version output {observed:?} does not end with a newline"
        ));
    }
    for line in rest.split_terminator('\n') {
        if !WHICH_COPY_LINE_PREFIXES
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            return Err(format!(
                "{name} --version output {observed:?} carries {line:?} after the identity \
                 line {wanted:?} — only the which-copy lines ({}) may follow it",
                WHICH_COPY_LINE_PREFIXES.join(", ")
            ));
        }
    }
    Ok(())
}

const WHICH_COPY_LINE_PREFIXES: &[&str] = &["running: ", "another copy: ", "build: ", "trusts: "];

/// Pure report validator used by the native-slice and final-universal runtime cross-checks.
pub fn validate_slice_update_pin_reports(
    expected: &str,
    reports: &[(&str, &str)],
) -> Result<(), String> {
    if !canonical_sha256(expected) {
        return Err("expected updater-pin fingerprint is not canonical lowercase SHA-256".into());
    }
    if reports.is_empty() {
        return Err("no architecture slice diagnostics were supplied".into());
    }
    for (label, report) in reports {
        let observed: Vec<&str> = report
            .lines()
            .filter_map(|line| line.strip_prefix("update-pin-sha256: "))
            .collect();
        let [observed] = observed.as_slice() else {
            return Err(format!(
                "architecture slice {label} reported {} update-pin-sha256 fields; expected exactly one",
                observed.len()
            ));
        };
        if !canonical_sha256(observed) || *observed != expected {
            return Err(format!(
                "architecture slice {label} updater pin {observed:?} differs from the permanent authority"
            ));
        }
    }
    Ok(())
}

fn verify_built_slice_update_pins(
    repo_root: &Path,
    slices: &[PathBuf],
    expected: &str,
) -> Result<(), String> {
    if slices.is_empty() {
        return Err("no architecture slices were supplied for updater-pin proof".into());
    }

    for slice in slices {
        let observed = read_thin_macho_update_pin(slice)?;
        validate_embedded_update_pin(expected, &observed, &slice.display().to_string())?;
    }

    let native = &slices[0];
    let output = command(native)
        .arg("--diagnose")
        .current_dir(repo_root)
        .output()
        .map_err(|error| {
            format!(
                "execute native architecture slice {} for updater-pin proof: {error}",
                native.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "native architecture slice {} --diagnose exited {}",
            native.display(),
            output.status
        ));
    }
    let report = String::from_utf8(output.stdout).map_err(|_| {
        format!(
            "native architecture slice {} diagnostics are not UTF-8",
            native.display()
        )
    })?;
    validate_slice_update_pin_reports(expected, &[("native architecture slice", &report)])
}

const X86_64_PROBE_SHELL: &str = aterm_uds::os_path!("zsh");

const SLICE_TURN_TEXT: &str = "uname -m";

const SLICE_SOCKET_WAIT: std::time::Duration = std::time::Duration::from_secs(180);

/// One architecture slice of the universal binary, as `arch(1)` selects it and `uname -m` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliceArch {
    Arm64,
    X86_64,
}

impl SliceArch {
    const fn arch_flag(self) -> &'static str {
        match self {
            SliceArch::Arm64 => "-arm64",
            SliceArch::X86_64 => "-x86_64",
        }
    }

    const fn uname(self) -> &'static str {
        match self {
            SliceArch::Arm64 => "arm64",
            SliceArch::X86_64 => "x86_64",
        }
    }

    const fn under(self) -> &'static str {
        match self {
            SliceArch::Arm64 => "natively",
            SliceArch::X86_64 => "under Rosetta",
        }
    }
}

/// The renderer a slice's session drive starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Renderer {
    /// `--cpu`: the x86_64 slice's drive under Rosetta at the build.
    Cpu,
    /// The launch default (GPU wanted), REQUIRED live.
    Gpu,
}

fn hermetic_env(command: &mut Command, home: &Path) {
    command
        .env_clear()
        .env("PATH", RELEASE_SYSTEM_PATH)
        .env("HOME", home);
}

fn x86_64_slice_runs(
    universal: &Path,
    home: &Path,
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> Result<String, String> {
    crate::gates::rosetta_runs(run)?;
    std::fs::create_dir_all(home).map_err(|error| format!("create {}: {error}", home.display()))?;
    let mut probe = command("/usr/bin/arch");
    probe
        .arg("-x86_64")
        .arg(universal)
        .arg("--diagnose")
        .current_dir("/");
    hermetic_env(&mut probe, home);
    probe.env("SHELL", X86_64_PROBE_SHELL);
    let out =
        run(&mut probe).map_err(|error| format!("run the x86_64 slice under Rosetta: {error}"))?;
    if !out.status.success() {
        return Err(format!(
            "the x86_64 slice did not run under Rosetta: `--diagnose` exited {} ({})",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout)
        .map_err(|_| "the x86_64 slice's diagnostics are not UTF-8".to_string())
}

/// A fresh, private (0700) scratch directory for a slice's probes (`--diagnose`, then the session turn), under `/tmp` so the control socket's path stays inside `sun_path`.
pub(crate) fn private_slice_scratch() -> Result<PathBuf, String> {
    aterm_tempfile::Builder::new()
        .prefix("aterm-sp-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .map(aterm_tempfile::TempDir::keep)
        .map_err(|error| format!("create private slice probe directory: {error}"))
}

/// A slice of `universal` DRIVES A SESSION.
pub(crate) fn slice_drives_a_session(
    universal: &Path,
    slice: SliceArch,
    renderer: Renderer,
    scratch: &Path,
    spawn: &mut dyn FnMut(&mut Command) -> std::io::Result<aterm_uds::spawnfd::Child>,
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Output>,
    socket_wait: std::time::Duration,
) -> Result<(), String> {
    struct Reap(aterm_uds::spawnfd::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let name = slice.uname();
    let home = scratch.join("home");
    let config = scratch.join("cfg");
    let runtime = scratch.join("run");
    for dir in [&home, &config.join("aterm"), &runtime] {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("create {}: {error}", dir.display()))?;
    }
    let toml = config.join("aterm").join("aterm.toml");
    crate::durable::write_fresh(
        &toml,
        "[update]\nenabled = false\n[packages]\nenabled = false\n",
    )
    .map_err(|error| format!("write {}: {error}", toml.display()))?;
    let sock = scratch.join("a.sock");
    let hermetic = |command: &mut Command| {
        hermetic_env(command, &home);
        command
            .env("SHELL", "/bin/sh")
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_RUNTIME_DIR", &runtime)
            .current_dir(&home);
    };

    let mut instance = command("/usr/bin/arch");
    instance
        .arg(slice.arch_flag())
        .arg(universal)
        .arg("--headless");
    if renderer == Renderer::Cpu {
        instance.arg("--cpu");
    }
    instance
        .args(["--no-reroute", "--control-sock"])
        .arg(&sock)
        .args(["--columns", "80", "--lines", "24"])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    hermetic(&mut instance);
    let _lifeline =
        aterm_uds::lifeline::Lifeline::arm(&mut instance, scratch).map_err(|error| {
            format!(
                "arm the {name} slice's lifeline in {}: {error}",
                scratch.display()
            )
        })?;
    let mut instance =
        Reap(spawn(&mut instance).map_err(|error| {
            format!("start the {name} slice headless {}: {error}", slice.under())
        })?);

    let started = Instant::now();
    while !sock.exists() {
        if let Ok(Some(status)) = instance.0.try_wait() {
            return Err(format!(
                "the {name} slice's headless instance exited ({status}) before its control \
                 socket appeared"
            ));
        }
        if started.elapsed() >= socket_wait {
            return Err(format!(
                "the {name} slice's headless instance opened no control socket within {}s",
                socket_wait.as_secs()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let ctl = |args: &[&str]| {
        let mut command = command("/usr/bin/arch");
        command
            .arg(slice.arch_flag())
            .arg(universal)
            .arg("ctl")
            .arg("--sock")
            .arg(&sock)
            .args(args);
        hermetic(&mut command);
        command
    };
    let settle = format!("settle=match:^{name}$");
    let mut turn_args = vec!["turn", settle.as_str(), "timeout=30000"];
    turn_args.extend(SLICE_TURN_TEXT.split(' '));
    let out = run(&mut ctl(&turn_args)).map_err(|error| {
        format!(
            "drive the {name} slice's session {}: {error}",
            slice.under()
        )
    })?;
    turn_verdict(&out, slice)?;
    if renderer == Renderer::Cpu {
        return Ok(());
    }
    let image = run(&mut ctl(&["image", "--bytes"]))
        .map_err(|error| format!("capture the {name} slice's first frame: {error}"))?;
    if !image.status.success() {
        return Err(format!(
            "the {name} slice drew no frame {}: `ctl image` exited {} ({})",
            slice.under(),
            image.status,
            String::from_utf8_lossy(&image.stderr).trim()
        ));
    }
    let metrics = run(&mut ctl(&["metrics"]))
        .map_err(|error| format!("read the {name} slice's metrics: {error}"))?;
    gpu_verdict(&metrics, slice)
}

fn gpu_verdict(out: &std::process::Output, slice: SliceArch) -> Result<(), String> {
    let name = slice.uname();
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("");
    let backend = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("backend="));
    match backend {
        Some("gpu") if out.status.success() => Ok(()),
        Some(live) if out.status.success() => Err(format!(
            "the {name} slice's pixels were drawn by the {live} renderer, not the requested \
             gpu one {}: a GPU renderer that does not start on the signed bytes (the hardened \
             runtime, library validation, the entitlements) ships a fallback no one asked for",
            slice.under()
        )),
        _ => Err(format!(
            "ctl metrics reported no backend field for the {name} slice ({}; exit {}), so \
             which renderer drew its pixels is unknown and this check decides nothing",
            line.trim(),
            out.status
        )),
    }
}

/// THE SIGNED BUNDLE DRIVES A SESSION ON THE GPU.
/// # Errors
/// The drive's refusal, naming what the turn saw.
pub(crate) fn signed_slice_drives_a_session(app: &Path) -> Result<(), String> {
    let scratch = private_slice_scratch()?;
    let driven = slice_drives_a_session(
        &app.join("Contents/MacOS/aterm"),
        SliceArch::Arm64,
        Renderer::Gpu,
        &scratch,
        &mut |command| command.spawn(),
        &mut |command| command.output(),
        SLICE_SOCKET_WAIT,
    );
    let _ = std::fs::remove_dir_all(&scratch);
    driven
}

fn turn_verdict(out: &std::process::Output, slice: SliceArch) -> Result<(), String> {
    let verdict = String::from_utf8_lossy(&out.stderr);
    let rows = String::from_utf8_lossy(&out.stdout);
    let settled = verdict
        .lines()
        .any(|line| line.contains(" turn ") && line.contains("status=settled"));
    let name = slice.uname();
    let answered = rows.lines().any(|row| row.trim() == name);
    if out.status.success() && settled && answered {
        return Ok(());
    }
    Err(format!(
        "the {name} slice did not drive a session {}: `{SLICE_TURN_TEXT}` was not answered \
         with {name} (client {}; {})",
        slice.under(),
        out.status,
        verdict.trim()
    ))
}

fn lipo_or_copy(slices: &[PathBuf], out: &Path) -> Result<(), String> {
    match slices {
        [] => Err("no architecture built".into()),
        [one] => {
            crate::durable::copy_regular_fresh(one, out)
                .map_err(|e| format!("copy {}: {e}", one.display()))?;
            make_executable(out)
        }
        many => {
            let mut cmd = command(aterm_uds::os_path!("lipo"));
            cmd.arg("-create").args(many).arg("-output").arg(out);
            run_quiet(cmd, "lipo -create")?;
            make_executable(out)
        }
    }
}

fn lipo_archs(bin: &Path) -> Result<String, String> {
    let out = command(aterm_uds::os_path!("lipo"))
        .arg("-archs")
        .arg(bin)
        .output()
        .map_err(|e| format!("spawn lipo -archs: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "lipo -archs failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn extract_dsym(
    bin: &Path,
    out_dir: &Path,
    short_version: &str,
    repo_root: &Path,
) -> Result<(Option<PathBuf>, Option<PathBuf>), String> {
    let dsym = out_dir.join("aterm.dSYM");
    let _ = std::fs::remove_dir_all(&dsym);
    let _ = command(aterm_uds::os_path!("dsymutil"))
        .arg(bin)
        .arg("-o")
        .arg(&dsym)
        .current_dir(repo_root)
        .output();

    let dwarf = dsym.join("Contents/Resources/DWARF/aterm");
    let ok = std::fs::symlink_metadata(&dwarf)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false);
    if !ok {
        println!("    WARNING: .dSYM empty/failed (crash reports won't symbolicate)");
        return Ok((None, None));
    }

    match (dwarf_uuids(bin), dwarf_uuids(&dwarf)) {
        (Some(bin_uuids), Some(dsym_uuids)) => {
            if !bin_uuids.iter().all(|u| dsym_uuids.contains(u)) {
                println!(
                    "    WARNING: dSYM UUIDs {dsym_uuids:?} don't cover binary UUIDs {bin_uuids:?} — discarding dSYM"
                );
                return Ok((None, None));
            }
            println!("    dSYM -> {} (UUID match)", dsym.display());
        }
        _ => println!(
            "    dSYM -> {} (dwarfdump unavailable; UUID check skipped)",
            dsym.display()
        ),
    }

    let zip = out_dir.join(format!("aterm-{short_version}-dSYM.zip"));
    let _ = std::fs::remove_file(&zip);
    let mut cmd = command("/usr/bin/ditto");
    cmd.arg("-c")
        .arg("-k")
        .arg("--keepParent")
        .arg(&dsym)
        .arg(&zip);
    run_quiet(cmd, "ditto dSYM zip")?;
    println!("    dSYM zip -> {}", zip.display());
    Ok((Some(dsym), Some(zip)))
}

fn dwarf_uuids(path: &Path) -> Option<Vec<String>> {
    let out = command(aterm_uds::os_path!("dwarfdump"))
        .arg("--uuid")
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let uuids: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("UUID: ")?
                .split_whitespace()
                .next()
                .map(String::from)
        })
        .collect();
    (!uuids.is_empty()).then_some(uuids)
}

/// chmod +x equivalent for a produced binary copy.
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let (file, meta) = crate::durable::open_regular_file_with_meta(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?;
    let mut perms = meta.permissions();
    let mode = perms.mode();
    if mode & 0o755 == 0o755 {
        return Ok(());
    }
    perms.set_mode(mode | 0o755);
    file.set_permissions(perms)
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

/// Windows: executability comes from the file extension; no mode bits to set.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn run_quiet(mut cmd: Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("spawn {what}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn target_bin(target_dir: &Path, triple: &str, bin: &str) -> PathBuf {
    target_dir.join(triple).join("release").join(bin)
}

fn fmt_elapsed(start: Instant) -> String {
    let s = start.elapsed().as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(all(test, unix))]
#[path = "slice_crash_tests.rs"]
mod crash_tests;

#[cfg(test)]
#[path = "buildplan_tests.rs"]
mod tests;
