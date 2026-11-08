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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::snapshot;

#[path = "linux_release.rs"]
pub mod linux;

const ARM64: &str = "aarch64-apple-darwin";
const X86_64: &str = "x86_64-apple-darwin";
const NATIVE_SLICE: &str = "native";
const COMPAT_CHECK_SLICE: &str = "x86_64-check";
const COMPAT_CHECK_EPOCH: u64 = 1;
const LIPO_ARM64: &str = "arm64";
const LIPO_X86_64: &str = "x86_64";

/// Platform policy, independent of the shared core pool and per-slice memory admission.
pub enum Sequencing {
    /// macOS slices borrow from one Release jobserver.
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
    /// Each slice's build: its triple, its wall seconds, and the peak resident memory its build child reported.
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
    /// Peak resident bytes of the build child and the children it waited for (`wait4`'s `ru_maxrss`), `None` where the host does not report it.
    pub peak_bytes: Option<u64>,
    /// CPU nanoseconds charged to the reaped build child and descendants it waited
    /// for. This is not wall time and excludes descendants abandoned without a wait.
    pub cpu_ns: Option<u64>,
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
        retire_moved_dirs(cache);
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

/// The build's folders where a take of an earlier version laid them, beside `snapshot/`
/// ([`snapshot::CacheRoot::moved_from`]): removed under this take's hold of the cache, which no
/// other take of the clone builds through, each said in a line; the stage only once the binary
/// an earlier build started there showed it ran ([`refuse_held_stage`]'s rule), else it stays for
/// a later take. What fails to go stays, said, and the take goes on.
fn retire_moved_dirs(cache: &snapshot::CacheRoot) {
    for old in cache.moved_from() {
        if !std::fs::symlink_metadata(&old).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if old.file_name() == Some(std::ffi::OsStr::new("stage"))
            && let Err(why) = refuse_held_stage(&old)
        {
            println!("    kept {}: {why}", old.display());
            continue;
        }
        match std::fs::remove_dir_all(&old) {
            Ok(()) => println!(
                "    removed {}: the build's folders are in {} now, out of fseventsd's stream",
                old.display(),
                cache.quiet().display()
            ),
            Err(e) => println!("    kept {}: it could not be removed ({e})", old.display()),
        }
    }
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

fn build_slices(
    plan: &BuildPlan,
    take: &Take,
    logs: &Path,
) -> Result<(Vec<PathBuf>, Vec<SliceBuild>), String> {
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
    // Both slices' claims are this process's: each slice's compilers claim their own memory only
    // where both slices' last units are on record ([`crate::slice_units::Units::of`]).
    let at_once = if matches!(plan.sequencing, Sequencing::OneAtATime) {
        1
    } else {
        2
    };
    let units = crate::slice_units::Units::live(&slices.map(|(triple, _, _)| triple), at_once);
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
            let units = units.iter().find(|u| u.triple() == triple);
            let need = crate::slice_units::slice_need(triple, target.is_none(), units);
            println!(
                "==> [{triple}] memory: claims {} for {}",
                aterm_capacity::size(need.mem),
                if units.is_some_and(crate::slice_units::Units::claims) {
                    "its driver; each compiler claims its unit's recorded peak, leading"
                } else {
                    "its whole build (no record of every slice's last unit yet; its compilers \
                     record theirs)"
                }
            );
            let memory = aterm_capacity::admit(need, aterm_capacity::Admit::Wait)
                .map_err(|e| e.to_string())?;
            if let Some(admitted) = admitted {
                let _ = admitted.send(());
            }
            println!(
                "==> [{triple}] memory waiting_s={:.3}",
                memory.waited().as_secs_f64()
            );
            let run = SliceRun {
                verb: "build",
                pkg: "aterm",
                target,
                target_dir: take.slice_target(target),
                build_number: plan.build_number,
                index_hint: plan.index_hint.as_deref(),
                static_crt: false,
                units,
            };
            run_slice_profile(take, &run, Some(&log), None, Some(pool))
        })();
        let secs = t.elapsed().as_secs();
        match built {
            Ok(usage) => {
                println!(
                    "==> [{triple}] done in {}{}",
                    fmt_elapsed(t),
                    usage.peak_bytes.map_or_else(String::new, |b| format!(
                        " (peak {} MiB resident)",
                        b / (1024 * 1024)
                    ))
                );
                Ok(SliceBuild {
                    triple,
                    secs,
                    peak_bytes: usage.peak_bytes,
                    cpu_ns: usage.cpu_ns,
                    units: usage.units,
                })
            }
            Err(error) => Err(format!("[{triple}] {error}\n{}", log_tail(&log, 40))),
        }
    };
    let results: Vec<Result<SliceBuild, String>> =
        if matches!(plan.sequencing, Sequencing::OneAtATime) {
            slices.into_iter().map(|slice| one(slice, None)).collect()
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

    let (slices, slice_builds) = build_slices(plan, take, &take.stage)?;
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
    /// A release slice's units and this executable, their compilers' wrapper
    /// ([`crate::slice_units::Units::env`]); `None` for every other build.
    units: Option<(&'a crate::slice_units::Units, &'a Path)>,
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
    if let Some((units, wrapper)) = e.units {
        env.extend(units.env(wrapper));
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
        .mem_gib(37)
        .cores(cores)
        .disk(30 * (1 << 30), root)
        .scratch()
}

/// Build one pilot arm with the same sealed source and environment as a cut.
/// Each target is new: a pilot never inherits Cargo intermediates from a cut.
pub(crate) fn pilot_build(
    take: &Take,
    arm: crate::profile_pilot::Arm,
    root: &Path,
    target: Option<&str>,
    build_number: u64,
    index_hint: Option<&str>,
) -> Result<aterm_json::Value, String> {
    snapshot::private_dir(root, "profile pilot take")?;
    let target_dir = root.join("build.noindex");
    std::fs::create_dir(&target_dir)
        .map_err(|e| format!("fresh pilot target {}: {e}", target_dir.display()))?;
    let log = root.join("build.log");
    let waiting = Instant::now();
    let claim = aterm_capacity::admit(
        pilot_need(arm, root, aterm_capacity::cores()),
        aterm_capacity::Admit::Wait,
    )
    .map_err(|e| e.to_string())?;
    let waiting_s = waiting.elapsed().as_secs_f64();
    let load_before = aterm_sysprobe::load_average();
    let started = Instant::now();
    let (usage, max_load1) = crate::profile_pilot::with_load(|| {
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
            Some((arm, claim.cores())),
            None,
        )
    });
    let usage = usage?;
    let wall = started.elapsed().as_secs_f64();
    take.verify()?;
    let binary = match target {
        Some(triple) => target_bin(&target_dir, triple, "aterm"),
        None => target_dir.join("release/aterm"),
    };
    let gui_rlib_to_binary_s = crate::profile_pilot::gui_link_interval(&log, &binary)?;
    let inspection = crate::profile_pilot::inspect(&binary, root).unwrap_or_else(|error| {
        aterm_json::Value::Object(aterm_json::Map::from([(
            "error".to_string(),
            aterm_json::Value::String(error),
        )]))
    });
    let (sha256, bytes) = crate::dmg::sha256_and_size(&binary)?;
    let retained = root.join("aterm");
    std::fs::rename(&binary, &retained).map_err(|e| format!("retain pilot artifact: {e}"))?;
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
            aterm_json::Value::String("wait4: reaped child and waited descendants".to_string()),
        ),
        ("binary".to_string(), aterm_json::json!(retained)),
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
        ("starved_s".to_string(), aterm_json::Value::Null),
        ("inspection".to_string(), inspection),
        ("log".to_string(), aterm_json::json!(log)),
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
    let mut cmd = command(driver);
    cmd.current_dir("/")
        .args(args)
        .env_clear()
        .envs(release_env(env))
        .stdin(aterm_uds::spawnfd::Stdio::null());
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
    run_slice_profile(take, run, log, None, None)
}

fn run_slice_profile(
    take: &Take,
    run: &SliceRun<'_>,
    log: Option<&Path>,
    arm: Option<(crate::profile_pilot::Arm, u32)>,
    pool: Option<&aterm_capacity::Jobserver>,
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
    // A slice's compilers start under this executable ([`crate::slice_units`]).
    let wrapper = run
        .units
        .map(|_| std::env::current_exe())
        .transpose()
        .map_err(|e| format!("this executable, the slice's compiler wrapper: {e}"))?;
    let mut cmd = release_build_command(
        &driver_path,
        cargo_build_args(
            verb,
            &take.workspace_config,
            &take.snapshot.sealed_config,
            &take.snapshot.src.join("Cargo.toml"),
            pkg,
            target,
            run.static_crt,
        ),
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
            units: run.units.zip(wrapper.as_deref()),
        },
    );
    if let Some((arm, cores)) = arm {
        cmd.envs(arm.environment())
            .env("CARGO_BUILD_JOBS", cores.to_string());
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
    let messages = take.temporary_dir.join(format!(
        "{verb}-{pkg}-{}-{}.messages.jsonl",
        target.unwrap_or("native"),
        std::process::id()
    ));
    let sink = crate::durable::create_fresh(&messages)
        .map_err(|e| format!("create {}: {e}", messages.display()))?;
    cmd.stdout(sink);
    let started = Instant::now();
    let spawned = cmd.spawn();
    cmd.stdout(aterm_uds::spawnfd::Stdio::null());
    let child = spawned.map_err(|e| format!("spawn {driver_name} for {pkg}: {e}"))?;
    let (status, mut usage) =
        wait_with_peak(child).map_err(|e| format!("wait for {driver_name} for {pkg}: {e}"))?;
    let wall = started.elapsed();
    usage.units = crate::durable::open_regular_file(&messages)
        .and_then(units_of)
        .inspect_err(|e| println!("==> {pkg}: cannot read Cargo's messages: {e}"))
        .ok();
    if let Some(log) = log.filter(|_| arm.is_some()) {
        let retained = log.with_extension("messages.jsonl");
        std::fs::rename(&messages, &retained)
            .map_err(|e| format!("retain pilot Cargo messages {}: {e}", retained.display()))?;
    } else {
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

/// The longest Cargo message line [`units_of`] reads; a longer one is skipped.
const MESSAGE_LINE_LIMIT: usize = 4 * 1024 * 1024;

/// Count the units Cargo's `messages` report ([`Units`]), each line read once and bounded, by
/// the reading a verify run gives its own calls ([`aterm_verify::compile_account`]).
/// # Errors
/// The read's.
fn units_of(mut messages: impl Read) -> std::io::Result<Units> {
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
/// nothing measured it. A slice shares its build child's process group with the other slice, so
/// no group read can split its starvation out.
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
    let bytes = usage.units.map_or_else(
        || "-".to_owned(),
        |u| format!("{} MiB", u.output_bytes / (1024 * 1024)),
    );
    format!(
        "compile {call}: {units}, {cpu} CPU-s, - starved-s, {:.1} s wall, peak {peak}, {bytes} written",
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
                            aterm_json::Value::String(
                                "reaped-child-and-waited-descendants".to_string(),
                            ),
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

/// Wait for `child` and retain its CPU and peak resident memory, including the
/// children it waited for (`wait4`'s rusage). These are accounting observations;
/// each slice admits its memory separately. CPU is separate from the wall clock.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn wait_with_peak(
    mut child: aterm_uds::spawnfd::Child,
) -> std::io::Result<(std::process::ExitStatus, BuildUsage)> {
    let usage = child.wait_with_rusage()?;
    Ok((
        usage.status,
        BuildUsage {
            peak_bytes: Some(usage.maxrss).filter(|b| *b > 0),
            cpu_ns: u64::try_from(usage.cpu.as_nanos()).ok(),
            units: None,
        },
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn wait_with_peak(
    mut child: aterm_uds::spawnfd::Child,
) -> std::io::Result<(std::process::ExitStatus, BuildUsage)> {
    Ok((child.wait()?, BuildUsage::default()))
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

static TEMP_SLICE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct PrivateSliceDir(PathBuf);

impl PrivateSliceDir {
    fn create() -> Result<Self, String> {
        for _ in 0..128 {
            let sequence = TEMP_SLICE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aterm-update-pin-proof-{}-{sequence}",
                std::process::id()
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "create private updater-pin proof directory: {error}"
                    ));
                }
            }
        }
        Err("could not allocate a unique updater-pin proof directory".into())
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
    use std::os::unix::fs::DirBuilderExt as _;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    for _ in 0..128 {
        let sequence = TEMP_SLICE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!(
            "/tmp/aterm-slice-probe-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("create {}: {error}", dir.display())),
        }
    }
    Err("could not allocate a unique slice probe directory under /tmp".into())
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

    #[test]
    fn release_slice_memory_waits_instead_of_overriding_available_memory() {
        for (native, gib) in [(true, 42), (false, 23)] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
            let machine =
                aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(128, gib - 1, 16));
            let need = crate::slice_units::slice_need("injected", native, None);
            assert_eq!(need.mem, gib << 30);
            assert!(
                aterm_capacity::admit::try_admit_with(&ledger, &machine, need.clone())
                    .unwrap()
                    .is_none()
            );
            machine.set(aterm_capacity::Reading::of_gib(128, gib + 1, 16));
            let claim = aterm_capacity::admit::try_admit_with(&ledger, &machine, need)
                .unwrap()
                .unwrap();
            assert!(claim.alone().is_none());
        }
    }

    /// Under the release's pool the slices make no core claim of their own and draw from that
    /// pool; with none handed, `ship build` makes its own; a pool named but unadoptable refuses.
    /// Negative control: a `slice_pool` that always calls `own` panics in the first case.
    #[test]
    fn slices_draw_from_a_handed_pool_without_a_core_claim_of_their_own() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let release = aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 3).unwrap();
        let handed = aterm_capacity::Jobserver::adopt(&release.handed()).unwrap();
        let Ok(chosen) = super::slice_pool(Some(Ok(handed)), || {
            panic!("a handed pool makes no claim of its own")
        }) else {
            panic!("the handed pool is adopted");
        };
        assert!(chosen.own.is_none());
        assert_eq!(chosen.waited, std::time::Duration::ZERO);
        assert_eq!(
            (chosen.pool.slots(), chosen.pool.flags()),
            (3, release.flags())
        );
        let held = chosen.pool.try_acquire().unwrap().expect("a token");
        let rest: Vec<_> = std::iter::from_fn(|| release.try_acquire().unwrap()).collect();
        assert_eq!(
            rest.len(),
            2,
            "the slices drew from the release's own tokens"
        );
        drop(rest);
        drop(held);

        let asked = std::cell::Cell::new(false);
        let own = super::slice_pool(None, || {
            asked.set(true);
            Err("own".into())
        });
        assert!(asked.get() && matches!(own, Err(why) if why == "own"));

        let refused = super::slice_pool(Some(Err(std::io::Error::other("no FIFO"))), || {
            panic!("a pool named but unadoptable is never replaced by a claim of every core")
        });
        assert!(
            matches!(&refused, Err(why) if why.contains(aterm_capacity::POOL_ENV) && why.contains("no FIFO"))
        );
    }

    /// The profile pilot's removable cold target is a scratch claim. Negative control: removing
    /// `.scratch()` from `pilot_need` makes this fail while leaving the generic builder green.
    #[test]
    fn profile_pilot_marks_its_cold_intermediates_as_scratch() {
        let root = std::path::Path::new("/pilot");
        let need = super::pilot_need(crate::profile_pilot::Arm::Fat1, root, 7);
        assert_eq!(need.what, "profile-pilot fat1");
        assert_eq!(need.mem, 37 * (1 << 30));
        assert_eq!(need.cores, 7);
        assert_eq!(need.disk, 30 * (1 << 30));
        assert_eq!(need.path.as_deref(), Some(root));
        assert!(
            need.scratch,
            "the pilot removes its cold target after the take"
        );
    }

    #[test]
    fn compat_compile_keeps_the_main_lanes_cpu_grant() {
        let mut cmd = aterm_uds::spawnfd::command("unused");
        super::compat_budget(&mut cmd, Some("1"), Some("3")).unwrap();
        let env: std::collections::BTreeMap<_, _> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(
            env.get("CARGO_BUILD_JOBS").and_then(Option::as_deref),
            Some("3")
        );
        assert_eq!(
            env.get("BUILD_SLOT_HELD").and_then(Option::as_deref),
            Some("1")
        );
        assert!(super::compat_budget(&mut cmd, Some("1"), Some("0")).is_err());
        assert!(super::compat_budget(&mut cmd, Some("1"), None).is_err());
        let mut ordinary = aterm_uds::spawnfd::command("unused");
        super::compat_budget(&mut ordinary, None, Some("3")).unwrap();
        assert_eq!(ordinary.get_envs().count(), 0);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_build_wait_keeps_status_and_measured_peak_rss() {
        let child = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", "exit 23"])
            .spawn()
            .unwrap();
        let (status, usage) = super::wait_with_peak(child).unwrap();
        assert_eq!(status.code(), Some(23));
        assert!(usage.peak_bytes.is_some_and(|bytes| bytes > 0), "{usage:?}");
        assert!(usage.cpu_ns.is_some(), "{usage:?}");
        let event = super::build_cpu_event(
            "build aterm native",
            "aterm",
            None,
            "build",
            status,
            super::BuildUsage {
                peak_bytes: usage.peak_bytes,
                cpu_ns: Some(8_000_000_000),
                units: Some(super::Units {
                    fresh: 3,
                    compiled: 2,
                    unknown: 1,
                    output_bytes: 4096,
                }),
            },
            std::time::Duration::from_secs(3),
            None,
        );
        let value: aterm_json::Value =
            aterm_json::from_str(event.strip_prefix("release-event: ").unwrap()).unwrap();
        assert_eq!(value["call"].as_str(), Some("build aterm native"));
        assert_eq!(value["units"]["fresh"].as_u64(), Some(3));
        assert_eq!(value["units"]["compiled"].as_u64(), Some(2));
        assert_eq!(value["units"]["unknown"].as_u64(), Some(1));
        assert_eq!(value["output_bytes"].as_u64(), Some(4096));
        assert_eq!(value["outcome"].as_str(), Some("FAIL"));
        assert_eq!(value["execution_s"].as_f64(), Some(3.0));
        assert_eq!(value["child_cpu"]["observed_s"].as_f64(), Some(8.0));
        assert_eq!(
            value["child_cpu"]["scope"].as_str(),
            Some("reaped-child-and-waited-descendants")
        );
        let unknown = super::build_cpu_event(
            "build aterm native",
            "aterm",
            None,
            "build",
            status,
            super::BuildUsage::default(),
            std::time::Duration::from_secs(3),
            None,
        );
        let value: aterm_json::Value =
            aterm_json::from_str(unknown.strip_prefix("release-event: ").unwrap()).unwrap();
        assert!(value["child_cpu"].is_null());
        assert!(value["units"].is_null() && value["output_bytes"].is_null());
        assert_eq!(
            super::compile_line(
                "build aterm native",
                super::BuildUsage::default(),
                std::time::Duration::from_secs(3)
            ),
            "compile build aterm native: - fresh / - compiled, - CPU-s, - starved-s, 3.0 s wall, peak -, - written"
        );
    }

    /// Cargo's messages count each unit once by `fresh`, a unit with no `fresh` as neither
    /// fresh nor built (verify's reading, `aterm_verify::compile_account`), and the bytes are
    /// the files of the units not reported fresh, each file once however many names it has; a
    /// fresh unit's files, a file that is gone, a symlink and a line that is not a
    /// `compiler-artifact` count nothing. NEGATIVE CONTROL: the symlink names a 5000-byte file,
    /// and a unit with no `fresh` read as built would count 6 built.
    #[test]
    fn the_units_count_fresh_and_built_and_size_only_what_was_built() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let lib = dir.path().join("libbuilt.rlib");
        std::fs::write(&lib, vec![0_u8; 1000]).unwrap();
        let meta = dir.path().join("libbuilt.rmeta");
        std::fs::write(&meta, vec![0_u8; 200]).unwrap();
        let uplifted = dir.path().join("aterm");
        std::fs::write(&uplifted, vec![0_u8; 30]).unwrap();
        let alias = dir.path().join("aterm-0123");
        std::fs::hard_link(&uplifted, &alias).unwrap();
        let fresh = dir.path().join("libfresh.rlib");
        std::fs::write(&fresh, vec![0_u8; 5000]).unwrap();
        let artifact = |files: &[&std::path::Path], fresh: bool| {
            aterm_json::json!({
                "reason": "compiler-artifact",
                "filenames": files.iter().map(|f| f.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                "fresh": fresh,
            })
            .to_string()
        };
        let gone = dir.path().join("gone.rlib");
        let link = dir.path().join("liblink.rlib");
        std::os::unix::fs::symlink(&fresh, &link).unwrap();
        let unsaid = dir.path().join("libunsaid.rlib");
        std::fs::write(&unsaid, vec![0_u8; 7]).unwrap();
        let messages = [
            artifact(&[&lib, &meta], false),
            artifact(&[&uplifted], false),
            artifact(&[&alias, &gone], false),
            artifact(&[&fresh], true),
            artifact(&[&lib], false),
            artifact(&[&link], false),
            aterm_json::json!({
                "reason": "compiler-artifact",
                "filenames": [unsaid.to_string_lossy().into_owned()],
            })
            .to_string(),
            r#"{"reason":"build-script-executed","filenames":["x"]}"#.to_owned(),
            "   Compiling aterm v0.1.0".to_owned(),
        ]
        .join("\n");
        let units = super::units_of(Cursor::new(messages.as_bytes())).unwrap();
        assert_eq!(
            units,
            super::Units {
                fresh: 1,
                compiled: 5,
                unknown: 1,
                output_bytes: 1237,
            }
        );
    }

    /// A stage whose shipped binary was started and never showed it ran is refused, not
    /// cleared, until its hold is spent (a restart here); one it ran from, or one nothing
    /// was started from, may go.
    #[test]
    fn a_stage_whose_shipped_binary_never_showed_it_ran_is_held() {
        use aterm_uds::deadline::{EXEC_STAMP, RAN, stamp_exec};
        let dir = aterm_tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let ship = super::ship_stage(&stage);
        std::fs::create_dir_all(&ship).unwrap();
        super::refuse_held_stage(&stage).expect("nothing was started from it");
        stamp_exec(&ship);
        let refused = super::refuse_held_stage(&stage).unwrap_err();
        assert!(refused.contains("never showed it ran"), "{refused}");
        std::fs::write(ship.join(RAN), "").unwrap();
        super::refuse_held_stage(&stage).expect("it ran");
        std::fs::remove_file(ship.join(RAN)).unwrap();
        std::fs::write(ship.join(EXEC_STAMP), u64::MAX.to_string()).unwrap();
        super::refuse_held_stage(&stage).expect("a restart spent the hold");
    }

    use std::ffi::OsString;

    use super::{
        BuildOutput, LC_SEGMENT_64, MACH_HEADER_64_LEN, MACH_MAGIC_64, MH_EXECUTE, PrivateSliceDir,
        RELEASE_SYSTEM_PATH, ReleaseEnv, Renderer, SECTION_64_LEN, SEGMENT_COMMAND_64_LEN,
        SliceArch, SliceBuild, cargo_build_args, current_release_uid, parse_thin_macho_update_pin,
        publish_verified_output, release_build_command, release_env, release_tool_path,
        resolve_release_rustup_shim_dir, slice_drives_a_session, turn_verdict,
        validate_app_version_reports, validate_cli_app_version, validate_embedded_update_pin,
        validate_final_slice_records, validate_lipo_architectures, validate_named_cli_app_version,
        validate_slice_update_pin_reports, x86_64_probe_asset_name, x86_64_probe_transcript,
        x86_64_slice_runs,
    };

    #[test]
    fn the_universal_cut_runs_its_x86_64_slice_under_rosetta() {
        use std::os::unix::process::ExitStatusExt as _;
        let output = |code: i32, stdout: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: b"bad CPU type in executable".to_vec(),
        };
        let bin = std::path::Path::new("/stage/ship/aterm");
        let argv = |command: &aterm_uds::spawnfd::Command| -> Vec<String> {
            std::iter::once(command.get_program())
                .chain(command.get_args())
                .map(|s| s.to_string_lossy().into_owned())
                .collect()
        };

        let scratch = std::env::temp_dir().join(format!(
            "aterm-x86-probe-test-diagnose-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        let home = scratch.join("home");

        let err = x86_64_slice_runs(bin, &home, &mut |_| Ok(output(1, ""))).unwrap_err();
        assert_eq!(err, crate::gates::ROSETTA_REFUSAL);

        let mut calls = 0;
        x86_64_slice_runs(bin, &home, &mut |_| {
            calls += 1;
            Ok(output(if calls == 1 { 0 } else { 134 }, ""))
        })
        .unwrap_err();

        let mut seen = Vec::new();
        let mut rendered = Vec::new();
        let mut home_writable = false;
        let report = x86_64_slice_runs(bin, &home, &mut |command| {
            seen.push(argv(command));
            rendered.push(format!("{command:?}"));
            home_writable = std::fs::write(home.join("probe"), b"").is_ok();
            Ok(output(0, "app-version: 0.93.0\n"))
        })
        .unwrap();
        let home_env = format!("HOME={:?}", home.display().to_string());
        assert!(
            rendered[1].starts_with("cd \"/\" && env -i ")
                && rendered[1].contains(&home_env)
                && rendered[1].contains("SHELL=\"/bin/zsh\"")
                && rendered[1].contains("PATH=\"/usr/bin:/bin:/usr/sbin:/sbin\""),
            "{}",
            rendered[1]
        );
        assert!(home_writable, "the probe HOME must exist and be writable");
        let _ = std::fs::remove_dir_all(&scratch);
        assert_eq!(report, "app-version: 0.93.0\n");
        assert_eq!(
            seen[1],
            vec![
                "/usr/bin/arch",
                "-x86_64",
                "/stage/ship/aterm",
                "--diagnose"
            ]
        );
    }

    #[test]
    fn the_universal_cut_drives_a_session_on_its_x86_64_slice() {
        use std::os::unix::process::ExitStatusExt as _;
        let bin = std::path::Path::new("/stage/ship/aterm");
        let wait = std::time::Duration::from_secs(60);
        let turn_out = |code: i32, verdict: &str, rows: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: rows.as_bytes().to_vec(),
            stderr: verdict.as_bytes().to_vec(),
        };
        let settled = "aterm-ctl: turn submitted=1 status=settled seq=4 id=1 dur_ms=188\n";
        let scratch_for = |name: &str| {
            let dir = std::env::temp_dir()
                .join(format!("aterm-x86-turn-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        };
        let fake_instance = |command: &mut aterm_uds::spawnfd::Command| {
            let args: Vec<String> = command
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert_eq!(&args[..2], ["-x86_64", "/stage/ship/aterm"]);
            for flag in ["--headless", "--cpu", "--no-reroute"] {
                assert!(args.iter().any(|a| a == flag), "{flag} missing: {args:?}");
            }
            assert!(
                args.windows(2).any(|w| w == ["--lifeline-fd", "0"]),
                "the translated instance holds the cutter's lifeline: {args:?}"
            );
            let rendered = format!("{command:?}");
            assert!(rendered.contains("env -i "), "{rendered}");
            let sock = &args[args.iter().position(|a| a == "--control-sock").unwrap() + 1];
            aterm_uds::spawnfd::command("/bin/sh")
                .arg("-c")
                .arg(format!("touch '{sock}'; exec sleep 120"))
                .spawn()
        };

        let dir = scratch_for("pass");
        let mut turn_argv = Vec::new();
        slice_drives_a_session(
            bin,
            SliceArch::X86_64,
            Renderer::Cpu,
            &dir,
            &mut |c| fake_instance(c),
            &mut |command| {
                turn_argv = command
                    .get_args()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                Ok(turn_out(0, settled, "sh$ uname -m\nx86_64\n\n"))
            },
            wait,
        )
        .unwrap();
        assert_eq!(
            turn_argv[..4],
            ["-x86_64", "/stage/ship/aterm", "ctl", "--sock"]
        );
        assert_eq!(
            turn_argv[5..],
            [
                "turn",
                "settle=match:^x86_64$",
                "timeout=30000",
                "uname",
                "-m"
            ]
        );
        let toml = std::fs::read_to_string(dir.join("cfg/aterm/aterm.toml")).unwrap();
        assert!(toml.contains("[update]\nenabled = false") && toml.contains("[packages]"));

        for (verdict, rows, code) in [
            (settled, "sh$ uname -m\narm64\n", 0),
            (
                "aterm-ctl: turn submitted=1 status=timeout seq=7\n",
                "sh$ uname -m\n",
                0,
            ),
            ("aterm-ctl: ERR no session\n", "", 1),
        ] {
            let dir = scratch_for("refuse");
            let err = slice_drives_a_session(
                bin,
                SliceArch::X86_64,
                Renderer::Cpu,
                &dir,
                &mut |c| fake_instance(c),
                &mut |_| Ok(turn_out(code, verdict, rows)),
                wait,
            )
            .unwrap_err();
            assert!(!err.is_empty());
        }
        assert!(turn_verdict(&turn_out(0, settled, "x86_64\n"), SliceArch::X86_64).is_ok());
        assert!(turn_verdict(&turn_out(0, settled, "arm64\n"), SliceArch::Arm64).is_ok());
        turn_verdict(&turn_out(0, settled, "x86_64\n"), SliceArch::Arm64)
            .expect_err("another slice's answer");
        let arm64_instance = |command: &mut aterm_uds::spawnfd::Command| {
            let args: Vec<String> = command
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert_eq!(&args[..2], ["-arm64", "/stage/ship/aterm"]);
            assert!(!args.iter().any(|a| a == "--cpu"), "{args:?}");
            let sock = &args[args.iter().position(|a| a == "--control-sock").unwrap() + 1];
            aterm_uds::spawnfd::command("/bin/sh")
                .arg("-c")
                .arg(format!("touch '{sock}'; exec sleep 120"))
                .spawn()
        };
        let gpu_drive = |name: &str, image: i32, metrics: &str| {
            let dir = scratch_for(name);
            let mut verbs = Vec::new();
            let driven = slice_drives_a_session(
                bin,
                SliceArch::Arm64,
                Renderer::Gpu,
                &dir,
                &mut |c| arm64_instance(c),
                &mut |command| {
                    let argv: Vec<String> = command
                        .get_args()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect();
                    assert_eq!(argv[..4], ["-arm64", "/stage/ship/aterm", "ctl", "--sock"]);
                    verbs.push(argv[5..].join(" "));
                    Ok(match argv[5].as_str() {
                        "turn" => turn_out(0, settled, "sh$ uname -m\narm64\n"),
                        "image" => turn_out(image, "", "OK 1\n80 24 3 AAAA\n"),
                        _ => turn_out(0, "", metrics),
                    })
                },
                wait,
            );
            (driven, verbs)
        };
        let (driven, verbs) = gpu_drive("arm64", 0, "backend=gpu frames=3 fx_under_frames=0\n");
        driven.unwrap();
        assert_eq!(
            verbs,
            [
                "turn settle=match:^arm64$ timeout=30000 uname -m",
                "image --bytes",
                "metrics"
            ]
        );
        let (driven, _) = gpu_drive("arm64-cpu", 0, "backend=cpu frames=3\n");
        driven.unwrap_err();
        let (driven, _) = gpu_drive("arm64-none", 0, "frames=3\n");
        driven.unwrap_err();
        let (driven, verbs) = gpu_drive("arm64-noframe", 1, "backend=gpu\n");
        driven.unwrap_err();
        assert_eq!(verbs.len(), 2, "no metrics read without a frame: {verbs:?}");

        let dir = scratch_for("exit");
        slice_drives_a_session(
            bin,
            SliceArch::X86_64,
            Renderer::Cpu,
            &dir,
            &mut |_| aterm_uds::spawnfd::command("/usr/bin/false").spawn(),
            &mut |_| panic!("no turn without a socket"),
            wait,
        )
        .unwrap_err();

        let dir = scratch_for("silent");
        let err = slice_drives_a_session(
            bin,
            SliceArch::X86_64,
            Renderer::Cpu,
            &dir,
            &mut |_| aterm_uds::spawnfd::command("/bin/sleep").arg("30").spawn(),
            &mut |_| panic!("no turn without a socket"),
            std::time::Duration::ZERO,
        )
        .unwrap_err();
        assert!(err.contains("opened no control socket"), "{err}");
        for name in [
            "pass",
            "refuse",
            "exit",
            "silent",
            "arm64",
            "arm64-cpu",
            "arm64-none",
            "arm64-noframe",
        ] {
            let _ = std::fs::remove_dir_all(
                std::env::temp_dir()
                    .join(format!("aterm-x86-turn-test-{name}-{}", std::process::id())),
            );
        }
    }

    const EXPECTED: &str = "529d8b60583fdc58b13afdba7050de6b21c0740b86dd87e5af769a2afb6c30f4";
    const WRONG: &str = "b8d47d9179feb56b1cbbe61c000b81f18d1ac152507d8abd320e2a2297890f1f";

    #[cfg(target_vendor = "apple")]
    #[used]
    #[unsafe(link_section = "__DATA,__aterm_upin")]
    static NATIVE_MACHO_TEST_RECORD: [u8; 64] =
        *b"529d8b60583fdc58b13afdba7050de6b21c0740b86dd87e5af769a2afb6c30f4";

    fn report(pin: &str) -> String {
        format!("aterm diagnostics\nupdate-pin-sha256: {pin}\nrenderer: gpu\n")
    }

    fn create_test_rustup_shims(directory: &std::path::Path) {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        for tool in ["cargo", "rustup"] {
            let path = directory.join(tool);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
    }

    /// THE COMPAT SLICE RUNS FROM THE ROW'S RUSTUP'S OWN DIRECTORY: the `rustup` row's
    /// answer (its stamped copy, beside its proxies) is the shim directory, canonical, and
    /// the build's PATH is it then the system's. No other directory stands in for it.
    /// NEGATIVE CONTROL: the same directory without its `cargo` is refused, not replaced.
    #[test]
    fn rustup_shim_resolver_takes_the_rows_rustups_directory_and_builds_a_closed_path() {
        let scratch = PrivateSliceDir::create().unwrap();
        let bin = scratch.0.join("tooling.noindex/rustup-1.29.2/bin");
        create_test_rustup_shims(&bin);
        let alias = scratch.0.join("alias");
        symlink(scratch.0.join("tooling.noindex"), &alias).unwrap();

        let resolved = resolve_release_rustup_shim_dir(
            &alias.join("rustup-1.29.2/bin/rustup"),
            current_release_uid().unwrap(),
        )
        .unwrap();
        assert_eq!(resolved, bin.canonicalize().unwrap());
        assert_eq!(
            release_tool_path(&resolved).unwrap(),
            std::ffi::OsString::from(format!("{}:{RELEASE_SYSTEM_PATH}", resolved.display()))
        );

        std::fs::remove_file(bin.join("cargo")).unwrap();
        let error =
            resolve_release_rustup_shim_dir(&bin.join("rustup"), current_release_uid().unwrap())
                .unwrap_err();
        assert!(error.contains("inspect release cargo shim"), "{error}");
        let bare = std::path::Path::new("rustup");
        assert!(
            resolve_release_rustup_shim_dir(bare, current_release_uid().unwrap())
                .unwrap_err()
                .contains("names no directory")
        );
    }

    #[test]
    fn rustup_shim_resolver_refuses_an_unsafe_directory() {
        let scratch = PrivateSliceDir::create().unwrap();
        let bin = scratch.0.join("bin");
        create_test_rustup_shims(&bin);
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o770)).unwrap();

        let error =
            resolve_release_rustup_shim_dir(&bin.join("rustup"), current_release_uid().unwrap())
                .unwrap_err();
        assert!(error.contains("group/other-writable"), "{error}");
    }

    #[test]
    fn rustup_shim_resolver_requires_owned_real_executables() {
        let scratch = PrivateSliceDir::create().unwrap();
        let bin = scratch.0.join("bin");
        create_test_rustup_shims(&bin);
        std::fs::remove_file(bin.join("cargo")).unwrap();
        symlink("rustup", bin.join("cargo")).unwrap();

        let error =
            resolve_release_rustup_shim_dir(&bin.join("rustup"), current_release_uid().unwrap())
                .unwrap_err();
        assert!(error.contains("not a real regular file"), "{error}");
    }

    #[test]
    fn both_release_arches_use_one_locked_offline_dependency_source() {
        let workspace_config =
            std::path::Path::new("/c-release.noindex/snapshot/src/.cargo/config.toml");
        let source_config = std::path::Path::new("/c-release.noindex/snapshot/.cargo/config.toml");
        let manifest = std::path::Path::new("/c-release.noindex/snapshot/src/Cargo.toml");
        let native = cargo_build_args(
            "build",
            workspace_config,
            source_config,
            manifest,
            "aterm",
            None,
            false,
        );
        let compat = cargo_build_args(
            "build",
            workspace_config,
            source_config,
            manifest,
            "aterm",
            Some("x86_64-apple-darwin"),
            false,
        );
        let text = |args: &[std::ffi::OsString]| {
            args.iter()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let native = text(&native);
        let compat = text(&compat);
        for args in [&native, &compat] {
            for config in [workspace_config, source_config] {
                assert!(
                    args.windows(2)
                        .any(|pair| pair == ["--config", config.to_str().unwrap()])
                );
            }
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--manifest-path", manifest.to_str().unwrap()])
            );
            assert!(
                args.iter()
                    .any(|arg| arg == "--message-format=json-render-diagnostics")
            );
            assert!(args.contains(&"--locked".to_owned()));
            assert!(args.contains(&"--offline".to_owned()));
            assert!(args.windows(2).any(|pair| pair == ["--bin", "aterm"]));
        }
        assert_eq!(native.first().map(String::as_str), Some("--unverified"));
        assert!(!compat.contains(&"--unverified".to_owned()));
        assert!(
            compat
                .windows(2)
                .any(|pair| pair == ["--target", "x86_64-apple-darwin"])
        );
        let crt = |args: &[String]| args.iter().any(|arg| arg.contains("crt-static"));
        assert!(
            !crt(&native) && !crt(&compat),
            "only a static build links the C library in"
        );
    }

    /// A slice keeps the features its own package selects: the workspace's feature unification,
    /// which verify's test builds take (`aterm_toolchain::build_cache::with_test_graph`),
    /// reaches no slice's argv, nor the two configs a slice reads (the sealed one and the
    /// checkout's, where only verify's alias carries the words). NEGATIVE CONTROL: the reader
    /// finds the words in a test build's argv and a `[resolver]` table in a config.
    #[test]
    fn a_release_slice_never_resolves_the_workspaces_features() {
        let config = std::path::Path::new("/c-release.noindex/snapshot/src/.cargo/config.toml");
        let manifest = std::path::Path::new("/c-release.noindex/snapshot/src/Cargo.toml");
        let unifies = |args: &[OsString]| {
            args.iter().any(|a| {
                a.to_str()
                    .is_some_and(|a| a.contains("feature-unification"))
            })
        };
        for target in [
            None,
            Some("x86_64-apple-darwin"),
            Some("aarch64-unknown-linux-gnu"),
        ] {
            for static_crt in [false, true] {
                for verb in ["build", "check"] {
                    let args = cargo_build_args(
                        verb, config, config, manifest, "aterm", target, static_crt,
                    );
                    assert!(!unifies(&args), "{args:?}");
                }
            }
        }
        let test_build: Vec<OsString> = aterm_toolchain::build_cache::with_test_graph(&[
            "--unverified".to_owned(),
            "test".to_owned(),
        ])
        .into_iter()
        .map(OsString::from)
        .collect();
        assert!(unifies(&test_build), "the reader sees the words");
        // A config line, outside comments and `[alias]` strings, that chooses the unification.
        let chooses = |text: &str| {
            let mut table = String::new();
            text.lines().map(str::trim).any(|line| {
                if line.starts_with('#') {
                    return false;
                }
                if let Some(head) = line.strip_prefix('[') {
                    table = head.trim_end_matches(']').trim().to_owned();
                    return table == "resolver";
                }
                table != "alias"
                    && (line.starts_with("resolver") || line.contains("feature-unification"))
            })
        };
        assert!(!chooses(crate::snapshot::SEALED_CONFIG));
        assert!(!chooses(include_str!("../../../.cargo/config.toml")));
        assert!(chooses("[resolver]\nfeature-unification = \"workspace\"\n"));
        assert!(chooses("[unstable]\nfeature-unification = true\n"));
    }

    #[test]
    fn a_static_build_links_the_c_library_into_its_target_alone() {
        let config = std::path::Path::new("/private/take/.cargo/config.toml");
        let manifest = std::path::Path::new("/private/take/Cargo.toml");
        let triple = "aarch64-unknown-linux-gnu";
        let text = |args: Vec<OsString>| -> Vec<String> {
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let args = text(cargo_build_args(
            "build",
            config,
            config,
            manifest,
            "aterm-bootstrap-verify",
            Some(triple),
            true,
        ));
        let wanted = aterm_bootstrap_verify::pin::static_build_config(triple);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--config", wanted.as_str()]),
            "{args:?}"
        );
        assert!(args.windows(2).any(|pair| pair == ["--target", triple]));
        assert!(!args.contains(&"--unverified".to_owned()));
        assert!(!args.contains(&"--bin".to_owned()));
        let native = text(cargo_build_args(
            "build", config, config, manifest, "aterm", None, true,
        ));
        assert!(
            !native.iter().any(|arg| arg.contains("crt-static")),
            "{native:?}"
        );
    }

    /// A RELEASE SLICE'S CARGO NAMES THIS EXECUTABLE ITS COMPILER WRAPPER AND GAINS NOTHING
    /// ELSE: its environment is the build's own plus the wrapper's variables
    /// ([`crate::slice_units::Units::env`]), which the wrapper removes again from the compiler it
    /// starts. NEGATIVE CONTROL: a build with no units names no wrapper.
    #[test]
    fn a_slices_cargo_names_this_executable_its_compiler_wrapper_and_gains_nothing_else() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let units =
            crate::slice_units::Units::of(&dir.path().join("units.json"), &[super::ARM64], 1)
                .remove(0);
        let wrapper = std::path::Path::new("/r/aterm-release");
        let env = |units| ReleaseEnv {
            path: "/t/bin:/usr/bin".into(),
            home: std::ffi::OsStr::new("/h"),
            tmp: std::path::Path::new("/private/take/tmp"),
            cargo_home: "/private/take/cargo".into(),
            build_number: 4242,
            short_commit: "0123456789ab",
            commit: "0123456789abcdef0123456789abcdef01234567",
            target_dir: "/private/take/target".into(),
            rustup_home: None,
            index_hint: None,
            units,
        };
        let plain = release_env(&env(None));
        let slice = release_env(&env(Some((&units, wrapper))));
        let mut gained: Vec<_> = slice.into_iter().filter(|kv| !plain.contains(kv)).collect();
        gained.sort();
        let mut want = units.env(wrapper);
        want.sort();
        assert_eq!(gained, want);
        assert!(
            want.iter()
                .any(|(k, v)| *k == "RUSTC_WRAPPER" && v == wrapper.as_os_str())
        );
        assert!(!plain.iter().any(|(k, _)| *k == "RUSTC_WRAPPER"));
    }

    #[test]
    fn the_release_build_enables_no_development_seam() {
        let config = std::path::Path::new("/private/take/.cargo/config.toml");
        let manifest = std::path::Path::new("/private/take/Cargo.toml");
        for target in [None, Some("x86_64-apple-darwin")] {
            let args: Vec<String> =
                cargo_build_args("build", config, config, manifest, "aterm", target, false)
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
            for arg in &args {
                assert!(
                    !arg.starts_with("--features")
                        && arg != "-F"
                        && arg != "--all-features"
                        && !arg.contains("dev-seams")
                        && !arg.contains("debug-assertions")
                        && !arg.contains("debug_assertions"),
                    "{target:?}: the release build names a feature or debug assertions: {args:?}"
                );
            }
            assert!(
                args.iter().any(|a| a == "--release")
                    && !args
                        .iter()
                        .any(|a| a == "--profile" || a.starts_with("--profile=")),
                "{target:?}: the cutter builds the `release` profile the pins below read: {args:?}"
            );
        }
        for (rustup_home, index_hint) in [
            (None, None),
            (None, Some("8..72")),
            (Some(OsString::from("/h/.rustup")), Some("8..72")),
        ] {
            let compat = rustup_home.is_some();
            let e = ReleaseEnv {
                path: "/t/bin:/usr/bin".into(),
                home: std::ffi::OsStr::new("/h"),
                tmp: std::path::Path::new("/private/take/tmp"),
                cargo_home: "/private/take/cargo".into(),
                build_number: 4242,
                short_commit: "0123456789ab",
                commit: "0123456789abcdef0123456789abcdef01234567",
                target_dir: "/private/take/target".into(),
                rustup_home,
                index_hint,
                units: None,
            };
            let cmd = release_build_command(
                std::path::Path::new("/t/bin/targo"),
                cargo_build_args("build", config, config, manifest, "aterm", None, false),
                &e,
            );
            let env: Vec<(String, Option<String>)> = cmd
                .get_envs()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.map(|v| v.to_string_lossy().into_owned()),
                    )
                })
                .collect();
            let mut want: Vec<(String, Option<String>)> = release_env(&e)
                .into_iter()
                .map(|(k, v)| (k.to_string(), Some(v.to_string_lossy().into_owned())))
                .collect();
            want.sort();
            assert_eq!(
                env, want,
                "the command's environment is release_env's, all of it"
            );
            assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new("/")));
            let get = |k: &str| {
                env.iter()
                    .find(|(name, _)| name == k)
                    .and_then(|(_, v)| v.clone())
            };
            for steering in [
                "RUSTFLAGS",
                "RUSTDOCFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
                "CARGO_BUILD_RUSTFLAGS",
                "RUSTC",
                "RUSTC_WRAPPER",
                "RUSTC_BOOTSTRAP",
                "CARGO_FEATURES",
            ] {
                assert_eq!(get(steering), None, "{steering} reaches the release build");
            }
            assert_eq!(
                get(aterm_stamp::input::MODE).as_deref(),
                Some(if cfg!(target_os = "macos") {
                    "postlink"
                } else {
                    "commit"
                })
            );
            assert_eq!(
                get(aterm_stamp::input::EPOCH).as_deref(),
                Some(if cfg!(target_os = "macos") {
                    "0"
                } else {
                    "4242"
                })
            );
            let (debug, strip) = if cfg!(target_os = "linux") {
                ("0", "debuginfo")
            } else {
                ("1", "false")
            };
            assert_eq!(get("CARGO_PROFILE_RELEASE_DEBUG").as_deref(), Some(debug));
            assert_eq!(get("CARGO_PROFILE_RELEASE_STRIP").as_deref(), Some(strip));
            assert_eq!(
                get(aterm_stamp::input::RELEASE).as_deref(),
                (!cfg!(target_os = "macos")).then_some("1")
            );
            assert_eq!(get(aterm_capacity::RELEASE_RUN_ENV).as_deref(), Some("1"));
            assert_eq!(
                get(aterm_stamp::input::INDEX_HINT).as_deref(),
                if cfg!(target_os = "macos") {
                    None
                } else {
                    index_hint
                }
            );
            if cfg!(target_os = "macos") {
                for key in [
                    aterm_stamp::input::COMMIT,
                    aterm_stamp::input::FULL_COMMIT,
                    aterm_stamp::input::DEV_COMMITS,
                ] {
                    assert_eq!(
                        get(key),
                        None,
                        "identity pins never reach a postlink compile"
                    );
                }
            }
            assert_eq!(
                get("RUSTUP_TOOLCHAIN").as_deref(),
                compat.then_some("stable"),
                "only the compat slice names a stock toolchain"
            );
        }

        let root = aterm_tempfile::manifest_dir().join("..").join("..");
        let mut manifests = vec![root.join("Cargo.toml")];
        for entry in std::fs::read_dir(root.join("crates"))
            .expect("crates/")
            .flatten()
        {
            if !entry
                .file_type()
                .map(|ft| ft.is_dir() || ft.is_symlink())
                .unwrap_or(true)
            {
                continue;
            }
            let path = entry.path().join("Cargo.toml");
            if path.is_file() {
                manifests.push(path);
            }
        }
        assert!(manifests.len() > 20, "found {} manifests", manifests.len());
        let mut forwarders = 0usize;
        for path in &manifests {
            let text = std::fs::read_to_string(path).expect("manifest");
            let doc: aterm_toml::Table =
                aterm_toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let shown = path.display();
            if let Some(features) = doc.get("features").and_then(|f| f.as_table()) {
                for (name, list) in features.iter() {
                    let names_seam = list.as_array().is_some_and(|items| {
                        items
                            .iter()
                            .any(|i| i.as_str().is_some_and(|v| v.contains("dev-seams")))
                    });
                    if name == "dev-seams" {
                        forwarders += 1;
                    } else {
                        assert!(
                            !names_seam,
                            "{shown}: feature `{name}` enables a development seam"
                        );
                    }
                }
            }
            let mut tables: Vec<&aterm_toml::Table> = Vec::new();
            for key in ["dependencies", "build-dependencies", "workspace"] {
                if let Some(t) = doc.get(key).and_then(|t| t.as_table()) {
                    tables.push(t);
                    if let Some(deps) = t.get("dependencies").and_then(|d| d.as_table()) {
                        tables.push(deps);
                    }
                }
            }
            if let Some(targets) = doc.get("target").and_then(|t| t.as_table()) {
                for spec in targets.values().filter_map(|v| v.as_table()) {
                    for key in ["dependencies", "build-dependencies"] {
                        if let Some(t) = spec.get(key).and_then(|t| t.as_table()) {
                            tables.push(t);
                        }
                    }
                }
            }
            for table in tables {
                for (dep, spec) in table.iter() {
                    let enables = spec
                        .as_table()
                        .and_then(|t| t.get("features"))
                        .and_then(|f| f.as_array())
                        .is_some_and(|items| items.iter().any(|i| i.as_str() == Some("dev-seams")));
                    assert!(!enables, "{shown}: `{dep}` is depended on with dev-seams");
                }
            }
        }
        assert!(forwarders > 0, "no crate declares the seam feature");

        let workspace: aterm_toml::Table =
            aterm_toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml")).unwrap())
                .unwrap();
        let release = workspace
            .get("profile")
            .and_then(|p| p.as_table())
            .and_then(|p| p.get("release"))
            .and_then(|r| r.as_table())
            .expect("the workspace declares [profile.release]");
        assert_no_debug_assertions("Cargo.toml [profile.release]", release);
        let config_path = root.join(".cargo/config.toml");
        let config: aterm_toml::Table =
            aterm_toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_no_debug_assertions(".cargo/config.toml", &config);
    }

    fn assert_no_debug_assertions(shown: &str, table: &aterm_toml::Table) {
        fn walk(shown: &str, path: &str, value: &aterm_toml::Value) {
            match value {
                aterm_toml::Value::Table(t) => {
                    for (key, v) in t.iter() {
                        let here = format!("{path}.{key}");
                        assert!(
                            !(key == "debug-assertions" && v.as_bool() != Some(false)),
                            "{shown}: `{here}` turns debug assertions on — and with them \
                             every development seam in a shipped binary"
                        );
                        walk(shown, &here, v);
                    }
                }
                aterm_toml::Value::Array(items) => {
                    for item in items {
                        walk(shown, path, item);
                    }
                }
                aterm_toml::Value::String(text) => {
                    for needle in ["debug-assertions", "debug_assertions", "dev-seams"] {
                        assert!(
                            !text.contains(needle),
                            "{shown}: `{path}` = {text:?} names {needle}"
                        );
                    }
                }
                _ => {}
            }
        }
        walk(shown, "", &aterm_toml::Value::Table(table.clone()));
    }

    #[test]
    fn a_verified_take_publishes_its_binary_and_symbols_out_of_the_stage() {
        let scratch = PrivateSliceDir::create().unwrap();
        let stage = scratch.0.join("stage");
        let out = scratch.0.join("dist");
        let dsym = stage.join("aterm.dSYM");
        let zip = stage.join("aterm-0.67.0-dSYM.zip");
        let app = stage.join("aterm.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        let binary = app.join("Contents/MacOS/aterm");
        std::fs::write(&binary, "stripped").unwrap();
        std::fs::create_dir_all(&dsym).unwrap();
        std::fs::write(dsym.join("symbol"), "verified").unwrap();
        std::fs::write(&zip, "archive").unwrap();
        let probe = stage.join(x86_64_probe_asset_name("0.67.0"));
        std::fs::write(&probe, x86_64_probe_transcript("0.67.0", "version=0.67.0")).unwrap();

        let published = publish_verified_output(
            BuildOutput {
                app: app.clone(),
                snapshot_digest: "d".repeat(64),
                slices: vec![SliceBuild {
                    triple: "aarch64-apple-darwin",
                    secs: 1,
                    peak_bytes: None,
                    cpu_ns: None,
                    units: None,
                }],
                archs: "x86_64 arm64".into(),
                compiler_line: "trust".into(),
                dsym: Some(dsym),
                dsym_zip: Some(zip),
                x86_64_probe: probe,
            },
            &out,
            current_release_uid().unwrap(),
        )
        .unwrap();

        assert_eq!(published.app, out.join("aterm.app"));
        assert_eq!(
            std::fs::read_to_string(out.join("aterm.app/Contents/MacOS/aterm")).unwrap(),
            "stripped"
        );
        assert!(
            !app.exists(),
            "the stage keeps no copy a later take could replace"
        );
        assert_eq!(published.dsym, Some(out.join("aterm.dSYM")));
        assert_eq!(published.dsym_zip, Some(out.join("aterm-0.67.0-dSYM.zip")));
        assert_eq!(
            published.x86_64_probe,
            out.join("aterm-0.67.0-x86_64-probe.txt"),
            "the x86_64 record lands beside the dSYM"
        );
        assert!(
            std::fs::read_to_string(out.join("aterm-0.67.0-x86_64-probe.txt"))
                .unwrap()
                .ends_with("version=0.67.0\n")
        );
        assert_eq!(
            std::fs::read_to_string(out.join("aterm.dSYM/symbol")).unwrap(),
            "verified"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("aterm-0.67.0-dSYM.zip")).unwrap(),
            "archive"
        );
        let out_metadata = std::fs::symlink_metadata(out).unwrap();
        assert_eq!(out_metadata.uid(), current_release_uid().unwrap());
        assert_eq!(out_metadata.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn a_verified_take_refuses_a_symlinked_output_directory() {
        let scratch = PrivateSliceDir::create().unwrap();
        let stage = scratch.0.join("stage");
        let outside = scratch.0.join("outside-dist");
        let out = scratch.0.join("dist");
        let dsym = stage.join("aterm.dSYM");
        std::fs::create_dir_all(&dsym).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let sentinel = outside.join("aterm.dSYM");
        std::fs::create_dir(&sentinel).unwrap();
        std::fs::write(sentinel.join("keep"), "outside").unwrap();
        symlink(&outside, &out).unwrap();

        let error = match publish_verified_output(
            BuildOutput {
                app: stage.join("aterm.app"),
                snapshot_digest: "d".repeat(64),
                slices: Vec::new(),
                archs: "x86_64 arm64".into(),
                compiler_line: "trust".into(),
                dsym: Some(dsym.clone()),
                dsym_zip: None,
                x86_64_probe: stage.join("probe.txt"),
            },
            &out,
            current_release_uid().unwrap(),
        ) {
            Ok(_) => panic!("symlinked release output was accepted"),
            Err(error) => error,
        };

        assert!(!error.is_empty());
        assert_eq!(
            std::fs::read_to_string(sentinel.join("keep")).unwrap(),
            "outside"
        );
        assert!(dsym.is_dir());
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_name(bytes: &mut [u8], offset: usize, value: &[u8]) {
        assert!(value.len() <= 16);
        bytes[offset..offset + value.len()].copy_from_slice(value);
    }

    fn thin_macho(sections: &[(&[u8], u64, &[u8])]) -> Vec<u8> {
        let section_count = u32::try_from(sections.len()).unwrap();
        let command_size = SEGMENT_COMMAND_64_LEN + SECTION_64_LEN * u64::from(section_count);
        let commands_end = MACH_HEADER_64_LEN + command_size;
        let data_size: u64 = sections
            .iter()
            .map(|(_, _, data)| u64::try_from(data.len()).unwrap())
            .sum();
        let file_len = commands_end + data_size;
        let mut bytes = vec![0_u8; usize::try_from(file_len).unwrap()];

        write_u32(&mut bytes, 0, MACH_MAGIC_64);
        write_u32(&mut bytes, 4, super::CPU_TYPE_ARM64);
        write_u32(&mut bytes, 12, MH_EXECUTE);
        write_u32(&mut bytes, 16, 1);
        write_u32(&mut bytes, 20, u32::try_from(command_size).unwrap());

        let segment = MACH_HEADER_64_LEN as usize;
        write_u32(&mut bytes, segment, LC_SEGMENT_64);
        write_u32(
            &mut bytes,
            segment + 4,
            u32::try_from(command_size).unwrap(),
        );
        write_name(&mut bytes, segment + 8, b"__DATA");
        write_u64(&mut bytes, segment + 40, commands_end);
        write_u64(&mut bytes, segment + 48, data_size);
        write_u32(&mut bytes, segment + 64, section_count);

        let mut data_offset = commands_end;
        for (index, (name, declared_size, data)) in sections.iter().enumerate() {
            let section =
                segment + SEGMENT_COMMAND_64_LEN as usize + index * SECTION_64_LEN as usize;
            write_name(&mut bytes, section, name);
            write_name(&mut bytes, section + 16, b"__DATA");
            write_u64(&mut bytes, section + 40, *declared_size);
            write_u32(
                &mut bytes,
                section + 48,
                u32::try_from(data_offset).unwrap(),
            );
            let end = data_offset + u64::try_from(data.len()).unwrap();
            bytes[usize::try_from(data_offset).unwrap()..usize::try_from(end).unwrap()]
                .copy_from_slice(data);
            data_offset = end;
        }
        bytes
    }

    fn parse_fixture(bytes: &[u8]) -> Result<String, String> {
        parse_thin_macho_update_pin(&mut Cursor::new(bytes), u64::try_from(bytes.len()).unwrap())
    }

    #[test]
    fn native_and_final_runtime_reports_must_match_exact_authority_pin() {
        let arm = report(EXPECTED);
        let x86 = report(EXPECTED);
        assert!(
            validate_slice_update_pin_reports(EXPECTED, &[("arm64", &arm), ("x86_64", &x86)])
                .is_ok()
        );

        let empty = report("empty");
        assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", &empty)]).is_err());
        let wrong = report(WRONG);
        assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", &wrong)]).is_err());
        assert!(
            validate_slice_update_pin_reports(EXPECTED, &[("arm64", &arm), ("x86_64", &wrong)])
                .is_err()
        );
        assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", "")]).is_err());
        let duplicate = format!("{}update-pin-sha256: {EXPECTED}\n", report(EXPECTED));
        assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", &duplicate)]).is_err());
    }

    #[test]
    fn app_version_reports_and_cli_identities_must_match_claim_exactly() {
        let report = "aterm diagnostics\nversion:   0.2.0 (abc123, built now)\nrenderer: gpu\n";
        assert!(validate_app_version_reports("0.2.0", &[("universal", report)]).is_ok());
        assert!(validate_app_version_reports("0.3.0", &[("universal", report)]).is_err());
        assert!(validate_app_version_reports("0.2.1", &[("universal", report)]).is_err());
        assert!(validate_app_version_reports("0.2.0", &[("universal", "")]).is_err());
        let duplicate = format!("{report}version:   0.2.0 (abc123, built now)\n");
        assert!(validate_app_version_reports("0.2.0", &[("universal", &duplicate)]).is_err());

        assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.0\n").is_ok());
        assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.1\n").is_err());
        assert!(validate_named_cli_app_version("aterm-gui", "0.2.0", b"aterm-gui 0.2.0\n").is_ok());
        assert!(validate_named_cli_app_version("aterm-ctl", "0.2.0", b"aterm-ctl 0.2.0\n").is_ok());
        assert!(
            validate_named_cli_app_version("aterm-ctl", "0.2.0", b"aterm-gui 0.2.0\n").is_err()
        );
        assert!(
            validate_cli_app_version(
                "0.2.0",
                b"aterm 0.2.0\nrunning: /Applications/aterm.app\nanother copy: \
                  /Users//ana/Applications/aterm.app (0.1.0) \xe2\x80\x94 the updater leaves \
                  it alone\n"
            )
            .is_ok()
        );
        assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.0\nrunning: /x/aterm\n").is_ok());
        let origin = aterm_types::identity::ORIGIN_LINE;
        assert!(
            validate_cli_app_version(
                "0.2.0",
                format!("aterm 0.2.0\n{origin}\nrunning: /x/aterm\n").as_bytes()
            )
            .is_ok()
        );
        assert!(
            validate_cli_app_version("0.2.0", format!("aterm 0.2.0\n{origin}\n").as_bytes())
                .is_ok()
        );
        assert!(
            validate_cli_app_version("0.2.0", b"aterm 0.2.0\nby somebody else\n").is_err(),
            "only the shared origin line, verbatim"
        );
        assert!(
            validate_cli_app_version(
                "0.2.0",
                format!("aterm 0.2.0\nrunning: /x/aterm\n{origin}\n").as_bytes()
            )
            .is_err(),
            "the origin line sits under the identity line, not after the copies"
        );
        assert!(
            validate_cli_app_version("0.2.1", b"aterm 0.2.0\nrunning: /x/aterm\n").is_err(),
            "the identity line is still exact"
        );
        assert!(
            validate_cli_app_version("0.2.0", b"aterm 0.2.0\nwarning: stale slice\n").is_err(),
            "anything but the which-copy lines still fails"
        );
        assert!(
            validate_cli_app_version("0.2.0", b"aterm 0.2.0\n\nrunning: /x/aterm\n").is_err(),
            "a blank line is not a which-copy line"
        );
        assert!(
            validate_cli_app_version("0.2.0", b"aterm 0.2.0\nrunning: /x/aterm").is_err(),
            "the report is newline-terminated"
        );
        assert!(
            validate_cli_app_version("0.2.0", b"running: /x/aterm\naterm 0.2.0\n").is_err(),
            "the identity line comes first"
        );
    }

    #[test]
    fn thin_macho_parser_accepts_one_exact_dedicated_record() {
        let bytes = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        let observed = parse_fixture(&bytes).expect("valid dedicated section");
        assert_eq!(observed, EXPECTED);
        validate_embedded_update_pin(EXPECTED, &observed, "arm64").unwrap();
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn parser_reads_the_real_native_test_macho_section() {
        let executable = std::env::current_exe().expect("current test executable");
        let observed = super::read_thin_macho_update_pin(&executable)
            .expect("parse dedicated section from the real native Mach-O test binary");
        assert_eq!(observed, EXPECTED);
    }

    #[test]
    fn thin_macho_parser_rejects_missing_duplicate_and_wrong_length_records() {
        let missing = thin_macho(&[]);
        assert!(parse_fixture(&missing).unwrap_err().contains("missing"));

        let duplicate = thin_macho(&[
            (b"__aterm_upin", 64, EXPECTED.as_bytes()),
            (b"__aterm_upin", 64, EXPECTED.as_bytes()),
        ]);
        assert!(parse_fixture(&duplicate).unwrap_err().contains("duplicate"));

        for declared_size in [63, 65] {
            let wrong_length = thin_macho(&[(b"__aterm_upin", declared_size, EXPECTED.as_bytes())]);
            assert!(parse_fixture(&wrong_length).unwrap_err().contains("length"));
        }
    }

    #[test]
    fn thin_macho_parser_rejects_wrong_segment_bounds_and_noncanonical_bytes() {
        let mut wrong_segment = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        let segment = MACH_HEADER_64_LEN as usize;
        wrong_segment[segment + 8..segment + 24].fill(0);
        write_name(&mut wrong_segment, segment + 8, b"__WRONG");
        assert!(
            parse_fixture(&wrong_segment)
                .unwrap_err()
                .contains("__DATA")
        );

        let mut outside = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        let section = segment + SEGMENT_COMMAND_64_LEN as usize;
        write_u32(&mut outside, section + 48, u32::MAX);
        assert!(parse_fixture(&outside).unwrap_err().contains("outside"));

        let uppercase = EXPECTED.to_ascii_uppercase();
        let noncanonical = thin_macho(&[(b"__aterm_upin", 64, uppercase.as_bytes())]);
        assert!(
            parse_fixture(&noncanonical)
                .unwrap_err()
                .contains("canonical")
        );

        let mut zero_fill = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        write_u32(&mut zero_fill, section + 64, 1);
        assert!(parse_fixture(&zero_fill).unwrap_err().contains("S_REGULAR"));
    }

    #[test]
    fn raw_fingerprint_substrings_are_never_authority() {
        let mut missing_with_decoy = thin_macho(&[]);
        missing_with_decoy.extend_from_slice(EXPECTED.as_bytes());
        assert!(parse_fixture(&missing_with_decoy).is_err());

        let mut wrong_record_with_decoy = thin_macho(&[(b"__aterm_upin", 64, WRONG.as_bytes())]);
        wrong_record_with_decoy.extend_from_slice(EXPECTED.as_bytes());
        let observed = parse_fixture(&wrong_record_with_decoy).unwrap();
        assert_eq!(observed, WRONG);
        assert!(validate_embedded_update_pin(EXPECTED, &observed, "x86_64").is_err());
    }

    #[test]
    fn malformed_thin_macho_metadata_fails_closed() {
        let valid = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);

        let mut fat_magic = valid.clone();
        write_u32(&mut fat_magic, 0, 0xcafe_babe);
        assert!(parse_fixture(&fat_magic).is_err());

        let mut truncated_commands = valid.clone();
        write_u32(&mut truncated_commands, 20, u32::MAX);
        assert!(parse_fixture(&truncated_commands).is_err());

        let mut impossible_count = valid;
        write_u32(&mut impossible_count, 16, u32::MAX);
        assert!(parse_fixture(&impossible_count).is_err());

        let mut wrong_segment_command = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        write_u32(
            &mut wrong_segment_command,
            MACH_HEADER_64_LEN as usize,
            super::LC_SEGMENT,
        );
        assert!(
            parse_fixture(&wrong_segment_command)
                .unwrap_err()
                .contains("32-bit")
        );
    }

    #[test]
    fn final_shipped_slice_proof_requires_exact_architecture_coverage() {
        let required = ["arm64", "x86_64"];
        assert!(
            validate_final_slice_records(
                EXPECTED,
                &required,
                &[("arm64", EXPECTED), ("x86_64", EXPECTED)],
            )
            .is_ok()
        );
        assert!(validate_final_slice_records(EXPECTED, &required, &[("arm64", EXPECTED)]).is_err());
        assert!(
            validate_final_slice_records(
                EXPECTED,
                &required,
                &[("arm64", EXPECTED), ("arm64", EXPECTED)],
            )
            .is_err()
        );
        assert!(
            validate_final_slice_records(
                EXPECTED,
                &required,
                &[("arm64", EXPECTED), ("x86_64", WRONG)],
            )
            .is_err()
        );
        assert!(
            validate_final_slice_records(
                EXPECTED,
                &required,
                &[("arm64", EXPECTED), ("ppc64", EXPECTED)],
            )
            .is_err()
        );

        assert!(validate_lipo_architectures("x86_64 arm64").is_ok());
        for invalid in ["arm64", "x86_64", "arm64 arm64", "arm64 x86_64 ppc64"] {
            assert!(validate_lipo_architectures(invalid).is_err(), "{invalid}");
        }
    }
}
