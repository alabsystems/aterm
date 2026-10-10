// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build plan (release spec §6 `buildplan.rs`): the release's ONE build (build
//! once, 2026-10-09 — `artifact.rs` keeps what it makes): both architecture slices
//! of `aterm` AT ONCE, each in a target directory and with a log of its own, with
//! `SOURCE_DATE_EPOCH=n` as the SOLE build-number conduit (zero build.rs changes —
//! spec decision 11), `lipo` to universal, then dSYM (success judged by DWARF file
//! existence — inherited dsymutil exit-code caveat — plus UUID match), `strip -x`
//! on the shipped copy, the dSYM zip, the x86_64 slice driven under Rosetta, and
//! the unsigned bundle laid out from the same snapshot (`bundle::assemble_at`).
//!
//! TWO compiler lanes and no third (owner decision, 2026-07): the native aarch64
//! slice on Trust (the repo's rust-toolchain.toml pin, built on targo's explicit
//! `--unverified` lane) and the x86_64-apple-darwin compat slice on
//! upstream stable. Both architectures build from one source take made from git
//! objects and one lock-checksummed, offline dependency bundle ([`crate::snapshot`]);
//! only their compiler differs. The ONLY
//! toolchain override is
//! `RUSTUP_TOOLCHAIN=stable` on the compat slice, set in `run_cargo` AFTER inherited
//! RUSTC / RUSTC_BOOTSTRAP / RUSTFLAGS / RUSTUP_TOOLCHAIN are scrubbed on both lanes:
//! no `RUSTC=…` pin, no RUSTFLAGS surgery, no `--no-trust` flag. The native binary
//! must still self-report `+t` (see [`run`]).
//!
//! The ONE exception: the x86_64-apple-darwin compat slice of the universal binary rides upstream
//! stable via `RUSTUP_TOOLCHAIN=stable`. STOCK EXCEPTION (Trust lacks an x86_64-apple-darwin std in
//! the aarch64 sysroot this lane builds with; MEASURED: `targo --unverified check --target
//! x86_64-apple-darwin -p aterm` → error[E0463] can't find crate for `std`). That pin lives HERE
//! and nowhere else, and the lane scrubs inherited RUSTC/RUSTFLAGS state so stale shell exports
//! cannot steer it.
//!
//! Preserved semantics:
//!   * `SOURCE_DATE_EPOCH` inherited by every cargo child so the binary's
//!     ATERM_BUILD_NUMBER == the plist CFBundleVersion == the manifest
//!     build_number, from ONE in-process u64 (spec §2 "propagation");
//!   * dsymutil success judged by the DWARF file's existence, NOT its exit
//!     code (see [`extract_dsym`]);
//!   * a failed cargo build is a hard error — a release artifact with a
//!     silently missing arch slice or missing atpkg/aterm-ctl/aterm-cli must
//!     be impossible.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::snapshot;

// Native workers share the snapshot of git objects and the scrubbed Trust
// invocation of the canonical cutter. They never sign or upload anything.
#[path = "linux_release.rs"]
pub mod linux;

const ARM64: &str = "aarch64-apple-darwin";
const X86_64: &str = "x86_64-apple-darwin";
/// The directory name of the slice built with no `--target` (the host's).
const NATIVE_SLICE: &str = "native";
const LIPO_ARM64: &str = "arm64";
const LIPO_X86_64: &str = "x86_64";

/// The compile-time marker every release build carries. The binary's
/// `BuildStamp.release_build` (`crates/aterm/src/main.rs`) is its presence (the
/// menu bar's release/dev signature); it carries no version.
pub const RELEASE_BUILD_MARKER: &str = "ATERM_RELEASE_BUILD";

/// Everything [`run`] needs, resolved by the caller (`artifact::build` owns the
/// identity and the store; this module only builds).
pub struct BuildPlan {
    /// The checkout whose repository holds [`BuildPlan::commit`] — release
    /// orchestration runs here. The build reads none of its files: the bytes come
    /// from git objects ([`crate::snapshot`]), and Cargo receives the snapshot's
    /// manifest explicitly from an inert root cwd.
    pub repo_root: PathBuf,
    /// The full commit the release builds — the reserved release commit for a
    /// release, HEAD for a dry run or rehearsal.
    pub commit: String,
    /// Receives what the build made, once the snapshot verified: the unsigned
    /// `aterm.app`, the dSYM and its zip, and the x86_64 record.
    pub out_dir: PathBuf,
    /// The reserved ledger number `n`, exported as `SOURCE_DATE_EPOCH` to every
    /// cargo child (→ ATERM_BUILD_NUMBER + ATERM_BUILD_TIME via the untouched
    /// build.rs — spec decision 11) and stamped as the bundle's CFBundleVersion.
    pub build_number: u64,
    /// Release version ("0.2.0") — names the dSYM zip `aterm-0.2.0-dSYM.zip`.
    pub short_version: String,
    /// Exact lowercase SHA-256 fingerprint that every independently-built and
    /// final shipped architecture slice must embed for its compiled updater
    /// key. `None` exists only for unsigned development/rehearsal fixtures.
    pub expected_update_pin_sha256: Option<String>,
    /// Build the slices one after the other, not at once: the memory the last
    /// build's slices peaked at would not fit beside each other
    /// (`artifact::slices_fit`).
    pub one_slice_at_a_time: bool,
}

/// What [`run`] produced: the unsigned universal bundle around the stripped,
/// ship-ready binary, plus the dSYM artifacts (symbols live in the dSYM, matched by
/// the Mach-O UUID, which strip preserves).
pub struct BuildOutput {
    /// The unsigned bundle (`<out_dir>/aterm.app`): `Contents/MacOS/aterm` is the
    /// stripped universal binary, its Info.plist stamped with `n` and the commit.
    pub app: PathBuf,
    /// The snapshot digest the bytes were built from ([`snapshot::Snapshot::digest`]).
    pub snapshot_digest: String,
    /// Each slice's build: its triple, its wall seconds, and the peak resident
    /// memory its build child reported (`wait4`), when it did.
    pub slices: Vec<SliceBuild>,
    /// `lipo -archs` of the shipped `aterm` (e.g. "x86_64 arm64") — for the
    /// cut transcript and a universal/single-arch sanity print.
    pub archs: String,
    /// The built binary's `--diagnose` `compiler:` line — provenance for the
    /// cut transcript (`rustc <release> (<slug>) · trust · release ·
    /// trust_verify …`). Always Trust-flavor: [`run`] hard-fails otherwise.
    pub compiler_line: String,
    /// `dist/aterm.dSYM` — present iff dsymutil produced a non-empty DWARF
    /// whose UUIDs match the binary (see the exit-code caveat on [`extract_dsym`]).
    pub dsym: Option<PathBuf>,
    /// `dist/aterm-<ver>-dSYM.zip` — the owner's symbolication archive, kept in `dist/`
    /// and never published (its DWARF names this machine's paths).
    pub dsym_zip: Option<PathBuf>,
    /// `aterm-<ver>-x86_64-probe.txt` — the x86_64 slice's `--diagnose` report under
    /// Rosetta ([`x86_64_probe_transcript`]), the record that the universal build RAN
    /// its Intel slice (docs/DESIGN-intel-just-works-2026-09-14.md §3.1). The site's
    /// `data-intel-proof` (`publish/site-sync.py`) links it once the public release
    /// carries it.
    pub x86_64_probe: PathBuf,
}

/// One architecture slice's build, as [`run`] measured it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceBuild {
    pub triple: &'static str,
    pub secs: u64,
    /// Peak resident bytes of the build child and the children it waited for
    /// (`wait4`'s `ru_maxrss`), `None` where the host does not report it.
    pub peak_bytes: Option<u64>,
}

/// The file name of a universal cut's x86_64 execution record.
#[must_use]
pub fn x86_64_probe_asset_name(version: &str) -> String {
    format!("aterm-{version}-x86_64-probe.txt")
}

/// The x86_64 execution record's text: what ran, what was checked, the session turn's
/// outcome, and the slice's own `--diagnose` report verbatim.
///
/// PII-free by construction: the report is produced under a cleared environment whose
/// `HOME` is a private scratch directory named by the cut's pid and a timestamp
/// ([`x86_64_slice_runs`]), so its `config:` and `environment:` lines name nothing of the
/// builder, and the session turn is recorded as a fixed sentence — never the screen,
/// whose prompt carries the host and user. That `HOME` is writable and `SHELL` is
/// [`X86_64_PROBE_SHELL`], as on a real Mac, so the report's `shell-int:` line reads the
/// x86_64 slice preparing its own shell loader, not the probe's environment.
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
    let output = Command::new("/usr/bin/id")
        .env_clear()
        .env("LC_ALL", "C")
        .arg("-u")
        .current_dir("/")
        .output()
        .map_err(|error| format!("inspect release user identity: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "inspect release user identity failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("release user identity is not UTF-8: {error}"))?
        .trim()
        .parse::<u32>()
        .map_err(|error| format!("release user identity is malformed: {error}"))
}

#[cfg(not(unix))]
pub(crate) fn current_release_uid() -> Result<u32, String> {
    Err("release target privacy requires Unix ownership and mode semantics".into())
}

/// Open one release-owned directory without accepting a symlink substitution.
///
/// The first metadata read rejects a static symlink. Opening and comparing the
/// device/inode pair before any chmod closes the check/use gap: if the path was
/// swapped while it was opened, no referent is modified. All later permission
/// changes apply to the opened directory descriptor, and the final path check
/// proves the same directory remains published at `path`.
///
/// The crate's one rule for a directory the release owns: every directory of the
/// release cache and the build's stage is made through [`snapshot::private_dir`],
/// which holds it to this.
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
    let directory =
        File::open(path).map_err(|error| format!("open {what} {}: {error}", path.display()))?;
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
    if private {
        let mut permissions = opened.permissions();
        permissions.set_mode(0o700);
        directory
            .set_permissions(permissions)
            .map_err(|error| format!("make {what} private {}: {error}", path.display()))?;
    }
    let after = directory
        .metadata()
        .map_err(|error| format!("reinspect opened {what} {}: {error}", path.display()))?;
    let published = std::fs::symlink_metadata(path)
        .map_err(|error| format!("reinspect {what} {}: {error}", path.display()))?;
    if !published.file_type().is_dir()
        || published.uid() != expected_uid
        || (published.dev(), published.ino()) != (after.dev(), after.ino())
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

const HOMEBREW_RUSTUP_SHIM_DIR: &str = "/opt/homebrew/opt/rustup/bin";
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

fn release_rustup_shim_dir(home: &std::ffi::OsStr) -> Result<PathBuf, String> {
    let home = Path::new(home);
    if !home.is_absolute() {
        return Err("release HOME must be absolute".into());
    }
    resolve_release_rustup_shim_dir(
        home,
        Path::new(HOMEBREW_RUSTUP_SHIM_DIR),
        current_release_uid()?,
    )
}

fn resolve_release_rustup_shim_dir(
    home: &Path,
    homebrew_fallback: &Path,
    expected_uid: u32,
) -> Result<PathBuf, String> {
    let standard = home.join(".cargo/bin");
    match std::fs::symlink_metadata(&standard) {
        Ok(_) => validate_release_rustup_shim_dir(&standard, expected_uid),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            validate_release_rustup_shim_dir(homebrew_fallback, expected_uid).map_err(|fallback| {
                format!(
                    "rustup shim directory is absent at {}; fallback failed: {fallback}",
                    standard.display()
                )
            })
        }
        Err(error) => Err(format!(
            "inspect standard rustup shim directory {}: {error}",
            standard.display()
        )),
    }
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
        let file = File::open(&path)
            .map_err(|error| format!("open release {tool} shim {}: {error}", path.display()))?;
        let opened = file.metadata().map_err(|error| {
            format!(
                "inspect opened release {tool} shim {}: {error}",
                path.display()
            )
        })?;
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

/// Rustup's home for the compat lane: a non-empty `$RUSTUP_HOME`, else
/// `<home>/.rustup` — atpkg's one rule ([`atpkg::seam::rustup_home_with`], which
/// calls `atpkg::toolchain::rustup_home`, the file aterm-verify compiles by
/// `#[path]` for the gate), which [`crate::gates`] resolves the rustup `trust`
/// entry through as well. A set-but-empty variable is the default, not a
/// relative path to refuse.
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

/// What a release build reads and writes: the [`snapshot::Snapshot`] of the commit
/// it builds, and the warm per-clone directories of its [`snapshot::CacheRoot`], held
/// for the take's whole life ([`snapshot::CacheHold`]): snapshot, both slices, lipo and
/// strip, [`Take::verify`], and the move of what it built out of the cache.
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
    /// `git rev-parse --short=12 C` — pinned as the binary's `ATERM_GIT_COMMIT` and
    /// stamped as the bundle's `ATermGitCommit`, so binary and plist name C alike.
    short_commit: String,
    /// The cache, this take's alone until the take drops (declared last, so it is let
    /// go of last).
    _hold: snapshot::CacheHold,
}

impl Take {
    /// Hold the clone's cache, materialize `commit` of `repo_root` into it and lay the
    /// build's directories beside it. Another take of the clone holding the cache is
    /// waited for ([`snapshot::CacheRoot::hold`]).
    pub(crate) fn prepare(repo_root: &Path, commit: &str) -> Result<Self, String> {
        let hold = snapshot::CacheRoot::for_repo(repo_root)?.hold()?;
        let cache = hold.cache();
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
    /// The slice's own `CARGO_TARGET_DIR`: `<target>/native` for the build with no
    /// `--target` (the host's Trust lane), `<target>/<triple>` for a `--target`
    /// one. Warm across builds, and each slice's alone.
    pub(crate) fn slice_target(&self, target: Option<&str>) -> PathBuf {
        self.target_root.join(target.unwrap_or(NATIVE_SLICE))
    }

    /// The slice's own `CARGO_HOME`, empty: an offline build over the vendored tree
    /// needs nothing in it, and two builds at once never share its locks.
    fn slice_cargo_home(&self, target: Option<&str>) -> PathBuf {
        self.cargo_home.join(target.unwrap_or(NATIVE_SLICE))
    }

    /// Whatever the build did, the bytes it read must still be the bytes digested
    /// before it: a source changed under cargo is a refusal, never a release.
    ///
    /// What cargo compiled from changed bytes is in the warm target, and cargo
    /// fingerprints a vendored package by its version, not its bytes — the next sync
    /// would restore the source and keep the artifact. So a refusal removes the target.
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

/// Run the whole build: snapshot → both slices' cargo builds, at once → lipo →
/// dsymutil → strip → dSYM zip → the x86_64 slice driven under Rosetta → the unsigned
/// bundle laid out from the snapshot, then the snapshot verified again and the
/// products moved out of the cache into `out_dir`, all under the take's hold on the
/// cache. Returns what it made, which no later take of the clone can touch.
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

/// Move what a verified take built out of its stage into `out_dir`: the unsigned
/// bundle, the dSYM (beside which its zip and the x86_64 record go). The stage is
/// the clone's, recreated by the next take, so nothing the release goes on to
/// measure or sign may stay in it.
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
    // Validate and tighten the output directory before any destructive
    // replacement below can be redirected outside it. (No `.metadata_never_index`
    // marker is written: it is inert for Spotlight, and its one reader went with
    // the sealed-seed client lane — `bundle::prepare_staging` says both.)
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

/// Move one staged release directory into `out_dir` under its own name, replacing
/// a stale copy.
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

/// Move one staged release file into `out_dir` under its own name, replacing a stale
/// copy from an earlier cut.
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

/// The slices' builds, AT ONCE (build once, 2026-10-09): the native slice on the
/// Trust lane and the x86_64 compat slice on upstream stable, each in its own
/// target directory ([`Take::slice_target`]), `CARGO_HOME` and log
/// (`<stage>/<triple>.log`, whose tail a failure prints), each line of the
/// transcript saying when it started and how long it took. Serial measured 7m04s
/// then 6m33s (0.101's cut log); at once they overlap. `plan.one_slice_at_a_time`
/// (the memory the last build's slices peaked at would not fit beside each other)
/// builds them one after the other, and says so.
///
/// Returns each slice's binary (native first) and what its build measured.
fn build_slices(
    plan: &BuildPlan,
    take: &Take,
    logs: &Path,
) -> Result<(Vec<PathBuf>, Vec<SliceBuild>), String> {
    // x86_64 compat slice: upstream stable via rustup's target std — THE one
    // exception to the single Trust lane. STOCK EXCEPTION (Trust lacks an
    // x86_64-apple-darwin std; E0463, see the module docs). Its target std is
    // asked for before either build starts, so a missing one refuses at once.
    require_rustup_target(X86_64)?;
    let slices: [(&'static str, Option<&'static str>, &str); 2] = [
        (ARM64, None, "Trust toolchain (+t): native build"),
        (
            X86_64,
            Some(X86_64),
            "upstream stable (+r): --target compat slice",
        ),
    ];
    let one = |(triple, target, lane): (&'static str, Option<&'static str>, &str)| {
        let log = logs.join(format!("{triple}.log"));
        let t = Instant::now();
        println!(
            "==> [{triple}] start {} — {lane} (SOURCE_DATE_EPOCH={}; log {})",
            clock_now(),
            plan.build_number,
            log.display()
        );
        let built = build_one(plan, take, "aterm", target, Some(&log));
        let secs = t.elapsed().as_secs();
        match built {
            Ok(peak_bytes) => {
                println!(
                    "==> [{triple}] done in {}{}",
                    fmt_elapsed(t),
                    peak_bytes.map_or_else(String::new, |b| format!(
                        " (peak {} MiB resident)",
                        b / (1024 * 1024)
                    ))
                );
                Ok(SliceBuild {
                    triple,
                    secs,
                    peak_bytes,
                })
            }
            Err(error) => Err(format!("[{triple}] {error}\n{}", log_tail(&log, 40))),
        }
    };
    let results: Vec<Result<SliceBuild, String>> = if plan.one_slice_at_a_time {
        println!(
            "==> the slices build one after the other: the memory their last builds \
             peaked at would not fit beside each other"
        );
        slices.into_iter().map(one).collect()
    } else {
        std::thread::scope(|scope| {
            let handles: Vec<_> = slices
                .into_iter()
                .map(|slice| scope.spawn(move || one(slice)))
                .collect();
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

/// `HH:MM:SS` of the wall clock, UTC, for a slice's start line.
fn clock_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let day = secs % 86_400;
    format!("{:02}:{:02}:{:02}Z", day / 3600, day % 3600 / 60, day % 60)
}

/// The last `n` lines of a slice's log, indented, for its failure.
fn log_tail(log: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| format!("    | {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_with_take(plan: &BuildPlan, take: &Take, symbol_out: &Path) -> Result<BuildOutput, String> {
    // Everything stays in the private stage until the snapshot has passed its
    // post-build verification. Only then is it moved into the output directory.
    snapshot::private_dir(symbol_out, "release symbol stage")?;

    // --- both slices, at once ------------------------------------------------
    // The native slice is the ordinary Trust lane, rooted in the snapshot
    // materialized above. No --target on purpose (host proc-macros and build
    // scripts ride the same config rustflags). Provenance remains hard-gated.
    let (slices, slice_builds) = build_slices(plan, take, &take.stage)?;

    if let Some(expected) = &plan.expected_update_pin_sha256 {
        verify_built_slice_update_pins(&plan.repo_root, &slices, expected)?;
        println!(
            "    updater pin: every architecture slice embeds {}…",
            &expected[..12]
        );
    }

    // --- lipo to universal ---------------------------------------------------
    let universal = take.stage.join("universal");
    snapshot::private_dir(&universal, "release universal stage")?;
    let fat = universal.join("aterm");
    lipo_or_copy(&slices, &fat)?;
    let archs = lipo_archs(&fat)?;
    println!("    universal binary: {archs}");

    // --- dSYM from the UN-stripped binary ----------------------------------
    let (dsym, dsym_zip) = extract_dsym(&fat, symbol_out, &plan.short_version, &plan.repo_root)?;

    // --- strip the SHIPPED copy -------------------------------------------
    // Symbols live in the archived .dSYM (matched by the Mach-O UUID, which
    // strip preserves); the bundle binary stays small while crash reports
    // remain symbolicatable. Stripping a private COPY keeps the unstripped
    // original available to dsymutil.
    let ship_dir = universal.join("ship");
    snapshot::private_dir(&ship_dir, "release ship stage")?;
    let shipped = ship_dir.join("aterm");
    std::fs::copy(&fat, &shipped).map_err(|e| format!("copy {}: {e}", fat.display()))?;
    make_executable(&shipped)?;
    // `strip -x`: local symbols only — matches build-app.sh; a failure here
    // is tolerated (`|| true` in the script) because an unstripped binary
    // is merely bigger, never wrong.
    let _ = Command::new("strip")
        .arg("-x")
        .arg(&shipped)
        .current_dir(&plan.repo_root)
        .output();

    // Bind the ACTUAL post-strip bytes, not merely cargo's pre-lipo inputs.
    // A tolerated strip failure still reaches this mandatory proof; a successful
    // strip/lipo that drops or changes either architecture's dedicated section
    // fails closed. Fat slices are extracted as data and never executed.
    if let Some(expected) = &plan.expected_update_pin_sha256 {
        verify_shipped_update_pin_slices(&shipped, &archs, expected)?;
        println!("    updater pin: every final shipped architecture structurally verified");
    }

    // --- compiler provenance HARD GATE ---------------------------------------
    // Ask the BUILT binary which compiler produced it: build.rs bakes the
    // `$RUSTC -vV` probe into `build_info::compiler_summary()`, and
    // `--diagnose` prints it as the `compiler:` line (`Trust <own version> ·
    // trustc <slug> · compatible with Rust <release> · <profile> · trust_verify
    // on|off`, or `Rust <release> · rustc <slug> · …` for upstream) — ground
    // truth for the shipped bytes. Single-lane invariant: the native slice
    // MUST be a Trust build (the row leads with `Trust`). Anything else means the toolchain
    // file was bypassed (broken rustup link, stale env) — that is a broken
    // toolchain, not a fallback, so the build refuses to continue.
    let diagnose = match Command::new(&shipped)
        .arg("--diagnose")
        .current_dir(&plan.repo_root)
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        _ => {
            return Err(
                "compiler provenance probe failed: the built aterm binary did not answer \
                 --diagnose on this host"
                    .to_string(),
            );
        }
    };
    if let Some(expected) = &plan.expected_update_pin_sha256 {
        validate_slice_update_pin_reports(expected, &[("final universal binary", &diagnose)])?;
        println!("    updater pin: final universal runtime cross-check passed");
    }
    validate_app_version_reports(
        &plan.short_version,
        &[("final universal binary", &diagnose)],
    )?;
    let version_output = Command::new(&shipped)
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
    // The row LEADS with the toolchain's own name: `Trust <own version> · trustc
    // <slug> · compatible with Rust <release> · …` for a Trust build, `Rust
    // <release> · rustc <slug> · …` for upstream (build_info::compiler_summary).
    if !compiler_line.starts_with("Trust") {
        return Err(format!(
            "compiler provenance gate: the native slice reports {compiler_line:?} — not a \
             Trust-flavor build. The repo compiles with Trust always; the native lane must \
             have been driven by something other than the stage2 targo/trustc (stale env, \
             wrong TRUST_STAGE2_BIN?). Fix the toolchain resolution and release again"
        ));
    }
    println!("    compiler: {compiler_line}  (Trust provenance gate passed)");
    // The host-registration line, echoed so the build transcript records what
    // THIS builder's frameworks register. A NOTE, not a gate: the builder's
    // macOS registering everything is what let v0.72.0-v0.75.0 ship green
    // while macOS 14.4.1 lacked `NSApplicationDelegate`; the fix is in the
    // objc layer (it supplies a missing protocol itself), and this line is
    // how a reader of the transcript can see which host the proof ran on.
    let objc_line = diagnose
        .lines()
        .find_map(|l| l.strip_prefix("objc:"))
        .map(str::trim)
        .unwrap_or("(no objc: line in --diagnose — binary predates the probe)");
    println!("    objc: {objc_line}");

    // --- THE x86_64 SLICE MUST RUN BEFORE A UNIVERSAL BUILD SHIPS ---------------
    // (docs/DESIGN-intel-just-works-2026-09-14.md §3.1.) Every Intel-only fault
    // the port found sat where an x86_64 EXECUTION differs from an arm64 one —
    // ObjC encodings, stret sends, `BOOL` width — which no build, type-check,
    // lint or structural pin proof can see. Rosetta reproduces that ABI exactly,
    // so the universal binary's x86_64 slice answers `--diagnose` under it, and
    // its updater pin and app version are cross-checked like the native one's.
    // No Rosetta is a refusal naming the remedy, never an unexecuted slice.
    let scratch = private_slice_scratch()?;
    let probed = (|| {
        let report = x86_64_slice_runs(&shipped, &scratch.join("home"), &mut |command| {
            command.output()
        })?;
        let label = "x86_64 slice under Rosetta";
        if let Some(expected) = &plan.expected_update_pin_sha256 {
            validate_slice_update_pin_reports(expected, &[(label, &report)])?;
        }
        validate_app_version_reports(&plan.short_version, &[(label, &report)])?;
        // `--diagnose` never reaches the session, PTY and CoreText paths, which is
        // where x86_64 stret/`BOOL` faults live: the slice must also DRIVE a session.
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
    // The record of the run, staged beside the dSYM and published with it: the proof
    // §3.1 promises the site gate, not a line that scrolled by.
    let record = symbol_out.join(x86_64_probe_asset_name(&plan.short_version));
    std::fs::write(
        &record,
        x86_64_probe_transcript(&plan.short_version, &report),
    )
    .map_err(|error| format!("write {}: {error}", record.display()))?;
    println!(
        "    x86_64 slice: ran under Rosetta — --diagnose answered, pin and version agree, \
         a headless session answered `{SLICE_TURN_TEXT}` with x86_64 (record: {})",
        x86_64_probe_asset_name(&plan.short_version)
    );

    // --- the unsigned bundle, laid out ONCE, from the same snapshot -------------
    // Its resources (the Info.plist template, the shell integration, the handoff
    // policy) are the release commit's own bytes, read from the snapshot this take
    // verifies after it. Signing is the cut's, over a byte copy of exactly this.
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
    workspace_config: &Path,
    source_config: &Path,
    manifest: &Path,
    pkg: &str,
    target: Option<&str>,
) -> Vec<OsString> {
    let mut args = Vec::with_capacity(16);
    if target.is_none() {
        args.push("--unverified".into());
    }
    args.extend([
        "--config".into(),
        workspace_config.as_os_str().to_owned(),
        "--config".into(),
        source_config.as_os_str().to_owned(),
        "build".into(),
        "--manifest-path".into(),
        manifest.as_os_str().to_owned(),
        "--release".into(),
        "--locked".into(),
        "--offline".into(),
        "-p".into(),
        pkg.into(),
    ]);
    if let Some(triple) = target {
        args.extend(["--target".into(), triple.into()]);
    }
    args
}

/// One `cargo build --release -p <pkg>` invocation. `target` = None for the
/// native Trust slice (the toolchain file supplies the compiler);
/// `target` = Some(triple) for the upstream-stable compat slice. Each slice builds
/// in its own target directory and `CARGO_HOME` ([`Take::slice_target`]), so two
/// can run at once.
///
/// Output goes to `log` when one is given (the slices build at once, and each has a
/// log of its own whose tail [`build_slices`] prints on a failure), else it streams
/// to the operator. Returns the peak resident memory the build child reported
/// (`wait4`), where the host reports it.
fn build_one(
    plan: &BuildPlan,
    take: &Take,
    pkg: &str,
    target: Option<&str>,
    log: Option<&Path>,
) -> Result<Option<u64>, String> {
    snapshot::refuse_root_cargo_config()?;
    // The build driver, per lane. Native slice: `targo` from the Trust stage2
    // tool dir — never a PATH `cargo`, a rustup shim the repo's toolchain pin
    // cannot satisfy. Compat
    // slice: upstream stable's `cargo` via the rustup shim — the ONE stock
    // lane, a STOCK EXCEPTION (the pinned Trust sysroot has no
    // x86_64-apple-darwin std: E0463; see the module docs).
    let home = std::env::var_os("HOME").ok_or("release build requires HOME")?;
    let compat_shim = if target.is_some() {
        Some(release_rustup_shim_dir(&home)?)
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
    // AT THE RELEASE'S OWN TIER — no `taskpolicy`. This is the one compile of the
    // bytes MEASURE judges and the cut signs (build once, 2026-10-09): the release's
    // critical path, under the machine's gate lock the release holds. At UTILITY a
    // peer's default-QoS build starved exactly this compile when it was MEASURE's
    // (0.100.0: 3943.6 s, `load 75.84 -> 53.53 on 18 cores`, the operator seeing it
    // at 2-4% of one core; `--compile-qos inherited`, 2026-09-30, went with the
    // gate's own release build). Under the release's lock the peers it competes
    // with as an equal are builds, not a person's typing.
    let mut cmd = Command::new(&driver_path);
    cmd.current_dir("/");
    // Targo's explicit unverified lane, the exact sealed dependency source,
    // and offline resolution are one tested argument vector for both arches.
    cmd.args(cargo_build_args(
        &take.workspace_config,
        &take.snapshot.sealed_config,
        &take.snapshot.src.join("Cargo.toml"),
        pkg,
        target,
    ));

    // Toolchain PATH, per lane. Native: the Trust stage2 bin dir first, so
    // targo resolves its co-located trustc/trustdoc (the physical dir —
    // protected Trust drivers refuse symlinked toolchain paths). Compat: the
    // validated, canonical rustup shim directory first, so stable supplies
    // the other Apple arch's std without an ambient PATH lookup.
    let tool_dir = if let Some(shim) = &compat_shim {
        shim
    } else {
        driver_path
            .parent()
            .ok_or("Trust build driver has no parent directory")?
    };
    let path = release_tool_path(tool_dir)?;
    cmd.env_clear();
    cmd.env("PATH", path)
        .env("HOME", &home)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TMPDIR", &take.temporary_dir)
        .env("CARGO_HOME", take.slice_cargo_home(target));

    // THE build-number conduit (spec §2 propagation): build.rs reads
    // SOURCE_DATE_EPOCH for ATERM_BUILD_NUMBER **and** ATERM_BUILD_TIME, and a
    // valid epoch WINS over its live-git fallback — so the binary, the plist
    // stamp, and the manifest all carry the one reserved u64.
    cmd.env("SOURCE_DATE_EPOCH", plan.build_number.to_string());
    // The commit stamp, PINNED: the snapshot has no `.git`, so crates/aterm/build.rs
    // takes these instead of asking git (and then watches no git path). The
    // release commit is the one its tag names, so it is 0 commits past it.
    cmd.env("ATERM_BUILD_GIT_COMMIT", &take.short_commit)
        .env("ATERM_BUILD_GIT_COMMIT_FULL", &take.snapshot.commit)
        .env("ATERM_BUILD_DEV_COMMITS", "0");
    cmd.env("CARGO_TARGET_DIR", take.slice_target(target));
    // Native Linux workers use fresh isolated targets and can run beside the
    // developer's app build. Bound their CPU/memory footprint explicitly after
    // env_clear; inheriting CARGO_BUILD_JOBS would not survive this boundary.
    #[cfg(target_os = "linux")]
    cmd.env("CARGO_BUILD_JOBS", "4");

    // Real .dSYM: line-table debug info AND no stripping (the release
    // profile's strip=true would erase the debug map dsymutil follows).
    // Scoped to THIS build via cargo profile env overrides — the global
    // profile is unchanged. Unlike build-app.sh's `:-` defaults these are set
    // UNCONDITIONALLY: an ambient CARGO_PROFILE_RELEASE_STRIP=true would
    // silently kill the dSYM, and a release builder must not be steerable by
    // stale shell state.
    cmd.env("CARGO_PROFILE_RELEASE_DEBUG", "1");
    cmd.env("CARGO_PROFILE_RELEASE_STRIP", "false");

    // NO trust anchors are injected into the child build: they are committed
    // constants (`aterm_update_core::pins`), and exporting them would make what a
    // binary trusts a property of the shell that compiled it.

    // Lane env. Native slice: NOTHING beyond the driver itself — the resolved
    // targo supplies trustc, its `--unverified` lane the verification opt-out,
    // and .cargo/config.toml (which targo reads, same discovery as cargo) the
    // Trust-lane cfg; adding env here would
    // create a second, undocumented lane. Compat slice: RUSTUP_TOOLCHAIN=stable
    // overrides the toolchain file (a deliberate stock lane, not a Trust
    // limitation — see the module docs), and inherited
    // RUSTC/RUSTC_BOOTSTRAP/RUSTFLAGS are scrubbed on BOTH lanes — a release
    // builder must not be steerable by stale shell state (same rule as the
    // CARGO_PROFILE pins above).
    cmd.env_remove("RUSTC")
        .env_remove("RUSTC_BOOTSTRAP")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTUP_TOOLCHAIN");
    // The release marker, and no version: the binary reports `[workspace.package]
    // version` as written, and `validate_cli_app_version` holds that to the
    // release being built.
    cmd.env(RELEASE_BUILD_MARKER, "1");
    let lane = if target.is_some() {
        cmd.env("RUSTUP_HOME", release_rustup_home(&home)?)
            .env("RUSTUP_TOOLCHAIN", "stable");
        "upstream stable (+r) compat"
    } else {
        "Trust (+t)"
    };

    let banner = format!(
        "==> {driver_name} build --release {}-p {pkg}  [{lane}]",
        target.map(|t| format!("--target {t} ")).unwrap_or_default()
    );
    cmd.stdin(std::process::Stdio::null());
    if let Some(log) = log {
        let mut file = File::create(log).map_err(|e| format!("create {}: {e}", log.display()))?;
        std::io::Write::write_all(&mut file, format!("{banner}\n").as_bytes())
            .map_err(|e| format!("write {}: {e}", log.display()))?;
        let err = file
            .try_clone()
            .map_err(|e| format!("open {}: {e}", log.display()))?;
        cmd.stdout(file).stderr(err);
    } else {
        println!("{banner}");
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn {driver_name} for {pkg}: {e}"))?;
    let (status, peak_bytes) =
        wait_with_peak(child).map_err(|e| format!("wait for {driver_name} for {pkg}: {e}"))?;
    if !status.success() {
        // Hard error: a warn-and-skip would let a release ship with a missing
        // slice.
        return Err(format!("{driver_name} build -p {pkg} failed ({status})"));
    }
    Ok(peak_bytes)
}

/// Wait for `child`, and read the peak resident memory of it and the children it
/// waited for (`wait4`'s `ru_maxrss`: bytes on macOS, KiB on Linux) — what the
/// build's own compilers held at their peak, which decides whether the next
/// build's slices may run at once.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn wait_with_peak(
    child: std::process::Child,
) -> std::io::Result<(std::process::ExitStatus, Option<u64>)> {
    use std::os::unix::process::ExitStatusExt as _;
    // `struct rusage` on every 64-bit Unix this crate builds on: two `timeval`s,
    // then fourteen `long`s, `ru_maxrss` the first.
    #[repr(C)]
    struct Rusage {
        utime: [i64; 2],
        stime: [i64; 2],
        maxrss: i64,
        rest: [i64; 13],
    }
    unsafe extern "C" {
        fn wait4(pid: i32, status: *mut i32, options: i32, rusage: *mut Rusage) -> i32;
    }
    let pid = i32::try_from(child.id()).map_err(std::io::Error::other)?;
    let mut status = 0i32;
    let mut usage = Rusage {
        utime: [0; 2],
        stime: [0; 2],
        maxrss: 0,
        rest: [0; 13],
    };
    loop {
        // SAFETY: `pid` is our own unreaped child; `status` and `usage` are live,
        // correctly laid-out locals the kernel writes for the duration of the call.
        let reaped = unsafe { wait4(pid, &raw mut status, 0, &raw mut usage) };
        if reaped == pid {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    // Reaped here: dropping the handle must not wait again.
    drop(child);
    let unit: u64 = if cfg!(target_os = "macos") { 1 } else { 1024 };
    let peak = u64::try_from(usage.maxrss)
        .ok()
        .filter(|b| *b > 0)
        .map(|b| b * unit);
    Ok((std::process::ExitStatus::from_raw(status), peak))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn wait_with_peak(
    mut child: std::process::Child,
) -> std::io::Result<(std::process::ExitStatus, Option<u64>)> {
    Ok((child.wait()?, None))
}

/// Refuse (with the exact remediation) when the stable toolchain's target std
/// is absent. Probes STABLE explicitly: the compat slice builds with
/// `RUSTUP_TOOLCHAIN=stable`, and a bare `rustup target list` here would
/// resolve the repo's `trust` toolchain (rust-toolchain.toml), which never
/// carries rustup-managed targets. Also refuses when rustup itself is missing.
fn require_rustup_target(triple: &str) -> Result<(), String> {
    let home = std::env::var_os("HOME").ok_or("compat target probe requires HOME")?;
    let rustup_home = release_rustup_home(&home)?;
    let shim = release_rustup_shim_dir(&home)?;
    let rustup = shim.join("rustup");
    let out = Command::new(&rustup)
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
             `rustup +stable target add {triple}` (the release is universal)"
        ));
    }
    Ok(())
}

/// The `commit-hash` stable's `rustc -vV` answers — the compiler of the x86_64 slice,
/// part of what a build is keyed by (`artifact::key`).
///
/// # Errors
/// No rustup shim, or a stable `rustc` that does not answer `-vV` with a commit.
pub(crate) fn stable_rustc_commit() -> Result<String, String> {
    let home = std::env::var_os("HOME").ok_or("the stable compiler probe requires HOME")?;
    let shim = release_rustup_shim_dir(&home)?;
    let out = checked_output(
        Command::new(shim.join("rustc"))
            .env_clear()
            .env("HOME", &home)
            .env("PATH", release_tool_path(&shim)?)
            .env("RUSTUP_HOME", release_rustup_home(&home)?)
            .env("RUSTUP_TOOLCHAIN", "stable")
            .arg("-vV")
            .current_dir("/"),
        "ask stable's rustc -vV",
    )?;
    commit_hash_of(&String::from_utf8_lossy(&out.stdout))
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

/// Parse one THIN 64-bit Mach-O executable and return the sole dedicated
/// updater-authority record.  This intentionally understands the small Mach-O
/// surface we emit instead of searching arbitrary executable bytes: a matching
/// fingerprint elsewhere (a diagnostic literal, debug data, or an attacker-added
/// decoy) is never authority.
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
    let mut file = File::open(path)
        .map_err(|error| format!("open architecture slice {}: {error}", path.display()))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("stat architecture slice {}: {error}", path.display()))?
        .len();
    parse_thin_macho_update_pin(&mut file, file_len)
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

/// The architectures a release ships: both, always (the universal app is the
/// product; there is no single-architecture release).
const SHIPPED_ARCHITECTURES: [&str; 2] = [LIPO_ARM64, LIPO_X86_64];

/// `lipo -archs`' answer for the shipped binary, held to [`SHIPPED_ARCHITECTURES`]
/// exactly — each once, nothing else.
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
            // `architecture` came through the exact allowlist above, so it is
            // both a safe filename component and a valid lipo selector.
            let thin = temp.0.join(architecture);
            let mut command = Command::new("lipo");
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

/// Require every diagnostics report to carry exactly one app-version field
/// equal to the ledger-derived release identity.
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
///
/// `aterm --version` says which copy runs after its identity line (S12 of
/// `docs/DESIGN-which-copy-runs-2026-08-27.md`): `running: <path>` and, per other
/// `aterm.app` in the usual places, `another copy: …`. Those lines are path-dependent
/// by design — a staged universal binary names its own path — so the gate pins the
/// identity line byte for byte and admits ONLY the S12 lines after it: anything else
/// (a stale cached library slice's chatter, alias-routing drift) still fails.
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
    // The origin line (`by Andrew Yates · ALab · alab.systems`) may sit right
    // under the identity line — exactly the shared constant, nothing else.
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

/// The only lines `aterm --version` may print after its identity line (and the
/// optional origin line right under it, admitted verbatim above): the S12
/// "which copy runs" report (`aterm_update::which_copy::WhichCopy::lines`), the
/// build number, and the trust roots this binary was built against.
///
/// `build:` and `trusts:` were added to `--version` after this gate was written,
/// so the v0.86.0 cut failed here on its own correct output — the gate was
/// refusing a line it had never been told about, which is the shape it exists to
/// catch and also the shape that makes it wrong when the report grows honestly.
/// Each is path- and machine-independent and carries no secret: `build:` is the
/// build number the identity already implies, and `trusts:` is the paper master's
/// fingerprint (`master=`), the same value the updater pin gate checks separately.
/// Anything ELSE after the identity line still fails.
const WHICH_COPY_LINE_PREFIXES: &[&str] = &["running: ", "another copy: ", "build: ", "trusts: "];

/// Pure report validator used by the native-slice and final-universal runtime
/// cross-checks. Each report must contain exactly one stable diagnostics field
/// and independently equal the authority.
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

    // Structural proof for EVERY thin slice.  In particular, the x86_64 path
    // is read as data and is never executed, so this gate works without Rosetta.
    for slice in slices {
        let observed = read_thin_macho_update_pin(slice)?;
        validate_embedded_update_pin(expected, &observed, &slice.display().to_string())?;
    }

    // Independent executable conformance for the native slice.  The final
    // stripped universal is checked again by `run`; neither probe executes the
    // x86_64 compatibility slice.
    let native = &slices[0];
    let output = Command::new(native)
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

/// The `SHELL` the x86_64 `--diagnose` probe runs with: the macOS default login shell,
/// so the report's `shell-int:` line exercises the slice's zsh loader-prepare path.
const X86_64_PROBE_SHELL: &str = "/bin/zsh";

/// What the session turn types into a slice's shell. A shell spawned by a process
/// runs that process's slice — a Rosetta-translated one its x86_64 slice — so the
/// screen answers the slice's own architecture ([`SliceArch::uname`]).
const SLICE_TURN_TEXT: &str = "uname -m";

/// How long a slice's headless instance may take to open its control socket: a hang
/// detector, never a budget. Rosetta translates the whole binary on the x86_64
/// slice's first run, which is most of this.
const SLICE_SOCKET_WAIT: std::time::Duration = std::time::Duration::from_secs(180);

/// One architecture slice of the universal binary, as `arch(1)` selects it and
/// `uname -m` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliceArch {
    Arm64,
    X86_64,
}

impl SliceArch {
    /// `arch`'s flag for the slice.
    fn arch_flag(self) -> &'static str {
        match self {
            SliceArch::Arm64 => "-arm64",
            SliceArch::X86_64 => "-x86_64",
        }
    }

    /// What `uname -m` prints in a shell the slice spawned.
    fn uname(self) -> &'static str {
        match self {
            SliceArch::Arm64 => "arm64",
            SliceArch::X86_64 => "x86_64",
        }
    }

    /// How the transcript names where the slice ran.
    fn under(self) -> &'static str {
        match self {
            SliceArch::Arm64 => "natively",
            SliceArch::X86_64 => "under Rosetta",
        }
    }
}

/// The renderer a slice's session drive starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Renderer {
    /// `--cpu`: the x86_64 slice's drive under Rosetta at the build, which judges the
    /// translated session, PTY and CoreText paths and keeps off the GPU.
    Cpu,
    /// The launch default (GPU wanted), REQUIRED live: the signed bundle's arm64 slice
    /// at the cut, where the hardened runtime, library validation and the entitlements
    /// signing added are what a GPU start meets. One clock-free pixel command (`ctl image
    /// --bytes`) starts the renderer — it starts lazily, on the first pixel — and `ctl
    /// metrics` must then report `backend=gpu`, never the CPU fallback.
    Gpu,
}

/// Scope `command` to a cleared environment: the system `PATH`, `HOME` = `home`, and
/// nothing else from the builder (its `ATERM_*`, its `HOME`, its user name).
fn hermetic_env(command: &mut Command, home: &Path) {
    command
        .env_clear()
        .env("PATH", RELEASE_SYSTEM_PATH)
        .env("HOME", home);
}

/// Run the x86_64 slice of `universal` under Rosetta (`/usr/bin/arch -x86_64 … --diagnose`)
/// and return its report. `run` spawns, injected so the refusal paths are testable without
/// Rosetta. `Err` when Rosetta cannot run a trivial x86_64 program
/// ([`crate::gates::ROSETTA_REFUSAL`] — the pre-claim [`crate::gates::universal_gate`] asks
/// the same probe first, so on a real cut this arm is Rosetta vanishing mid-build), or the
/// slice does not answer. The report is the record's body, so the slice runs in a cleared
/// environment, from `/`, with `home` (created here, inside the cut's private scratch) as
/// `HOME` and [`X86_64_PROBE_SHELL`] as `SHELL`: nothing of the builder can reach it, and
/// the slice prepares its shell loader in `home` as it would on a real Mac.
fn x86_64_slice_runs(
    universal: &Path,
    home: &Path,
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Output>,
) -> Result<String, String> {
    crate::gates::rosetta_runs(run)?;
    std::fs::create_dir_all(home).map_err(|error| format!("create {}: {error}", home.display()))?;
    let mut probe = Command::new("/usr/bin/arch");
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

/// A fresh, private (0700) scratch directory for a slice's probes (`--diagnose`, then
/// the session turn), under `/tmp` so the control socket's path stays inside `sun_path`.
/// Its name carries the builder's pid and a timestamp — nothing of the builder.
pub(crate) fn private_slice_scratch() -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt as _;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let dir = PathBuf::from(format!(
        "/tmp/aterm-slice-probe-{}-{nanos}",
        std::process::id()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|error| format!("create {}: {error}", dir.display()))?;
    Ok(dir)
}

/// A slice of `universal` DRIVES A SESSION — §3.1's second half for the x86_64 slice
/// under Rosetta at the build ([`Renderer::Cpu`]), and the signed bundle's arm64 slice
/// at the cut's self-check ([`Renderer::Gpu`]: the hardened runtime and entitlements
/// are what signing adds, and a launch and a GPU start are what reach them). A
/// headless, hermetic instance (`--headless --no-reroute`, `--cpu` for
/// [`Renderer::Cpu`], `HOME`/XDG dirs under `scratch`, updates and packages off) is
/// spawned under `arch <slice>`; once its control socket appears, one `aterm ctl turn`
/// — the slice's own client — types [`SLICE_TURN_TEXT`] and settles on a row reading
/// the slice's architecture, and for [`Renderer::Gpu`] one pixel command starts the
/// renderer and `ctl metrics` must name it the GPU ([`gpu_verdict`]). CLOCK-FREE: the
/// waits are hang detectors, never a latency budget. `spawn` and `run` are injected so every refusal is testable without
/// Rosetta. The instance is killed and reaped on every path, and it holds the
/// caller's LIFELINE (`aterm_uds::lifeline`, its FIFO in `scratch`): a release killed
/// before its `Drop` runs — a SIGKILL, a second Ctrl-C — takes the instance and its
/// shell with it (gap #36).
pub(crate) fn slice_drives_a_session(
    universal: &Path,
    slice: SliceArch,
    renderer: Renderer,
    scratch: &Path,
    spawn: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Child>,
    run: &mut dyn FnMut(&mut Command) -> std::io::Result<std::process::Output>,
    socket_wait: std::time::Duration,
) -> Result<(), String> {
    struct Reap(std::process::Child);
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
    std::fs::write(
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

    let mut instance = Command::new("/usr/bin/arch");
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    hermetic(&mut instance);
    // Held until this function returns, and bound BEFORE the `Reap` so it drops
    // after it: the instance is killed and reaped first, then the lifeline is cut.
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

    // The slice's own client, under `arch <slice>`, hermetic.
    let ctl = |args: &[&str]| {
        let mut command = Command::new("/usr/bin/arch");
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
    // The renderer starts lazily, on the first pixel command: one capture starts it (no
    // clock — the capture answers once a frame is drawn), and the live backend is read
    // after it, so it is the renderer that drew.
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

/// Judge the renderer that drew the slice's first frame from `ctl metrics`' first line
/// — `metrics::backend_gpu()`, the LIVE backend, stored on both arms of the GPU start
/// (`--diagnose` prints only the intent). `backend=gpu` passes; the CPU fallback (the
/// GPU did not start, and aterm-gui fell back silently) and a line with no backend are
/// refused, in the paint probe's own words.
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

/// THE SIGNED BUNDLE DRIVES A SESSION ON THE GPU: the cut's self-check runs
/// [`slice_drives_a_session`] on the native (arm64) slice of the bundle at `app` after
/// it is signed — the drive the build gives the x86_64 slice, over the bytes signing
/// changed — and requires the GPU renderer live ([`Renderer::Gpu`]). Its scratch is
/// private and removed on every path.
///
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

/// Judge the session turn: the client succeeded, its verdict (stderr) says the screen
/// SETTLED on the awaited row, and a row (stdout) reads exactly the slice's
/// architecture. Anything else is a refusal naming what the turn saw, never a pass.
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

/// Combine slices into one fat binary, or pass a single slice through
/// (build-app.sh's lipo step, incl. the single-arch copy branch).
fn lipo_or_copy(slices: &[PathBuf], out: &Path) -> Result<(), String> {
    match slices {
        [] => Err("no architecture built".into()), // unreachable: builds hard-fail
        [one] => {
            std::fs::copy(one, out).map_err(|e| format!("copy {}: {e}", one.display()))?;
            make_executable(out)
        }
        many => {
            let mut cmd = Command::new("lipo");
            cmd.arg("-create").args(many).arg("-output").arg(out);
            run_quiet(cmd, "lipo -create")?;
            make_executable(out)
        }
    }
}

/// `lipo -archs <bin>` → e.g. "x86_64 arm64".
fn lipo_archs(bin: &Path) -> Result<String, String> {
    let out = Command::new("lipo")
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

/// Extract `dist/aterm.dSYM` from the UN-stripped universal binary, then zip
/// it as `dist/aterm-<ver>-dSYM.zip` for the release assets.
///
/// EXIT-CODE CAVEAT (inherited from build-app.sh, preserved on purpose):
/// dsymutil exits non-zero on harmless "unable to open object file" warnings
/// (cargo's deleted intermediate .o's), so success is judged by the DWARF
/// file's existence and non-emptiness, NOT by the exit code. A missing/empty
/// dSYM is a WARNING, not an abort — the release still ships, crash reports
/// just won't symbolicate (same tolerance as the script).
fn extract_dsym(
    bin: &Path,
    out_dir: &Path,
    short_version: &str,
    repo_root: &Path,
) -> Result<(Option<PathBuf>, Option<PathBuf>), String> {
    let dsym = out_dir.join("aterm.dSYM");
    let _ = std::fs::remove_dir_all(&dsym);
    // Exit code + output deliberately ignored (see caveat above).
    let _ = Command::new("dsymutil")
        .arg(bin)
        .arg("-o")
        .arg(&dsym)
        .current_dir(repo_root)
        .output();

    let dwarf = dsym.join("Contents/Resources/DWARF/aterm");
    let ok = std::fs::metadata(&dwarf)
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    if !ok {
        println!("    WARNING: .dSYM empty/failed (crash reports won't symbolicate)");
        return Ok((None, None));
    }

    // UUID match (spec §6): the dSYM only symbolicates if its per-arch UUIDs
    // cover the binary's (strip preserves the UUID, so checking the unstripped
    // original vouches for the shipped copy too). Best-effort: a missing
    // dwarfdump skips the check rather than failing the cut.
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

    // ditto -c -k --keepParent: the standard .dSYM archive form (preserves the
    // bundle structure so Xcode/symbolicators accept it after unzip).
    let zip = out_dir.join(format!("aterm-{short_version}-dSYM.zip"));
    let _ = std::fs::remove_file(&zip);
    let mut cmd = Command::new("/usr/bin/ditto");
    cmd.arg("-c")
        .arg("-k")
        .arg("--keepParent")
        .arg(&dsym)
        .arg(&zip);
    run_quiet(cmd, "ditto dSYM zip")?;
    println!("    dSYM zip -> {}", zip.display());
    Ok((Some(dsym), Some(zip)))
}

/// `dwarfdump --uuid <path>` → the UUID token of every "UUID: <hex> (<arch>)"
/// line, or None when dwarfdump is unavailable / produced nothing.
fn dwarf_uuids(path: &Path) -> Option<Vec<String>> {
    let out = Command::new("dwarfdump")
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
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {e}", path.display()))
}

/// Windows: executability comes from the file extension; no mode bits to set.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Run a short command with captured output; on failure surface its stderr in
/// the error (the "typed Command shell-outs with captured stderr" rule, §6).
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

/// Where a `--target` build put a cargo binary (by its bin name, `aterm`: the one
/// binary is the whole surface, and the bundle adds its argv0 aliases as symlinks).
fn target_bin(target_dir: &Path, triple: &str, bin: &str) -> PathBuf {
    target_dir.join(triple).join("release").join(bin)
}

/// "4m12s" / "38s" — per-step timing for the cut transcript (spec §6).
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

    use super::{
        BuildOutput, LC_SEGMENT_64, MACH_HEADER_64_LEN, MACH_MAGIC_64, MH_EXECUTE, PrivateSliceDir,
        RELEASE_SYSTEM_PATH, Renderer, SECTION_64_LEN, SEGMENT_COMMAND_64_LEN, SliceArch,
        SliceBuild, cargo_build_args, current_release_uid, parse_thin_macho_update_pin,
        publish_verified_output, release_tool_path, resolve_release_rustup_shim_dir,
        slice_drives_a_session, turn_verdict, validate_app_version_reports,
        validate_cli_app_version, validate_embedded_update_pin, validate_final_slice_records,
        validate_lipo_architectures, validate_named_cli_app_version,
        validate_slice_update_pin_reports, x86_64_probe_asset_name, x86_64_probe_transcript,
        x86_64_slice_runs,
    };

    /// §3.1 of DESIGN-intel-just-works: a universal cut RUNS its x86_64 slice. A builder
    /// without Rosetta is refused with the remedy (and the escape) named, before the slice
    /// is ever tried; a slice that does not answer is refused; one that answers returns
    /// its report — and every spawn goes through `arch -x86_64`.
    #[test]
    fn the_universal_cut_runs_its_x86_64_slice_under_rosetta() {
        use std::os::unix::process::ExitStatusExt as _;
        let output = |code: i32, stdout: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: b"bad CPU type in executable".to_vec(),
        };
        let bin = std::path::Path::new("/stage/ship/aterm");
        let argv = |command: &std::process::Command| -> Vec<String> {
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

        let mut seen = Vec::new();
        let err = x86_64_slice_runs(bin, &home, &mut |command| {
            seen.push(argv(command));
            Ok(output(1, ""))
        })
        .unwrap_err();
        assert_eq!(err, crate::gates::ROSETTA_REFUSAL);
        assert!(err.contains("softwareupdate --install-rosetta") && !err.contains("--arm64-only"));
        assert_eq!(
            seen,
            vec![vec!["/usr/bin/arch", "-x86_64", "/usr/bin/true"]]
        );

        let mut calls = 0;
        let err = x86_64_slice_runs(bin, &home, &mut |_| {
            calls += 1;
            Ok(output(if calls == 1 { 0 } else { 134 }, ""))
        })
        .unwrap_err();
        assert!(err.contains("did not run under Rosetta"), "{err}");

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
        // PII-free by construction: the report that becomes the record runs in a
        // cleared environment under a HOME that names nobody, from `/`. That HOME is
        // writable and SHELL is zsh, as on a real Mac, so the report's `shell-int:`
        // line describes the slice, not a read-only probe HOME with no shell (the
        // `/var/empty` probe read `NOT ACTIVE — loader cache unwritable`).
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
        // The record a universal cut keeps of that run: a header saying what ran and
        // what was checked, then the slice's own report verbatim, newline-terminated.
        assert_eq!(
            x86_64_probe_asset_name("0.93.0"),
            "aterm-0.93.0-x86_64-probe.txt"
        );
        let record = x86_64_probe_transcript("0.93.0", &report);
        assert!(record.starts_with("# aterm 0.93.0: the universal binary's x86_64 slice, RUN"));
        assert!(
            record.contains("a private scratch HOME, SHELL=/bin/zsh")
                && record.contains("`uname -m` typed")
        );
        assert!(record.ends_with("\n\napp-version: 0.93.0\n"), "{record}");
        assert!(x86_64_probe_transcript("0.93.0", "no newline").ends_with("no newline\n"));
    }

    /// §3.1's session turn: the x86_64 slice runs headless under Rosetta in a hermetic
    /// scratch tree, and one translated `aterm ctl turn` must SETTLE on a row reading
    /// `x86_64`. An instance that dies or never opens its socket is refused, and so is a
    /// turn that timed out or saw another architecture (the negative controls). The
    /// instance is reaped on every path.
    #[test]
    fn the_universal_cut_drives_a_session_on_its_x86_64_slice() {
        use std::os::unix::process::ExitStatusExt as _;
        let bin = std::path::Path::new("/stage/ship/aterm");
        // A hang detector for `sh` + `touch` (a loaded box can take seconds to
        // start a shell); it returns as soon as the socket file appears.
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
        // The fake instance: the real command's --control-sock operand becomes a file,
        // then it sleeps until reaped — standing in for the translated headless aterm.
        // Its sleep outlives `wait`, so it can only end by the reap.
        let fake_instance = |command: &mut std::process::Command| {
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
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("touch '{sock}'; exec sleep 120"))
                .spawn()
        };

        // A turn that settles on `x86_64` passes, and the instance is gone afterwards.
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

        // Negative controls: the screen answered another architecture; the turn timed
        // out; the client failed.
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
            assert!(err.contains("did not drive a session"), "{err}");
        }
        assert!(turn_verdict(&turn_out(0, settled, "x86_64\n"), SliceArch::X86_64).is_ok());
        // The signed bundle's arm64 slice (the cut's self-check) is the same drive
        // under `arch -arm64`, judged by its own answer: `arm64` passes it, and the
        // x86_64 answer that passes the Rosetta drive refuses it.
        assert!(turn_verdict(&turn_out(0, settled, "arm64\n"), SliceArch::Arm64).is_ok());
        let wrong = turn_verdict(&turn_out(0, settled, "x86_64\n"), SliceArch::Arm64)
            .expect_err("another slice's answer");
        assert!(
            wrong.contains("arm64 slice did not drive a session natively"),
            "{wrong}"
        );
        // The signed arm64 drive wants the GPU: no `--cpu`, and after the turn one
        // pixel command starts the renderer and `metrics` must name it the GPU.
        let arm64_instance = |command: &mut std::process::Command| {
            let args: Vec<String> = command
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert_eq!(&args[..2], ["-arm64", "/stage/ship/aterm"]);
            assert!(!args.iter().any(|a| a == "--cpu"), "{args:?}");
            let sock = &args[args.iter().position(|a| a == "--control-sock").unwrap() + 1];
            std::process::Command::new("/bin/sh")
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
        // Negative controls: the CPU fallback drew the frame; metrics named no backend;
        // the capture failed (no frame, no renderer to judge).
        let (driven, _) = gpu_drive("arm64-cpu", 0, "backend=cpu frames=3\n");
        let err = driven.unwrap_err();
        assert!(
            err.contains("drawn by the cpu renderer, not the requested gpu one"),
            "{err}"
        );
        let (driven, _) = gpu_drive("arm64-none", 0, "frames=3\n");
        let err = driven.unwrap_err();
        assert!(err.contains("reported no backend field"), "{err}");
        let (driven, verbs) = gpu_drive("arm64-noframe", 1, "backend=gpu\n");
        let err = driven.unwrap_err();
        assert!(err.contains("drew no frame"), "{err}");
        assert_eq!(verbs.len(), 2, "no metrics read without a frame: {verbs:?}");

        // An instance that exits before its socket appears is refused, naming that.
        let dir = scratch_for("exit");
        let err = slice_drives_a_session(
            bin,
            SliceArch::X86_64,
            Renderer::Cpu,
            &dir,
            &mut |_| std::process::Command::new("/usr/bin/false").spawn(),
            &mut |_| panic!("no turn without a socket"),
            wait,
        )
        .unwrap_err();
        assert!(
            err.contains("exited") && err.contains("before its control socket"),
            "{err}"
        );

        // One that stays up but never opens it is refused at the bound.
        let dir = scratch_for("silent");
        let err = slice_drives_a_session(
            bin,
            SliceArch::X86_64,
            Renderer::Cpu,
            &dir,
            &mut |_| std::process::Command::new("/bin/sleep").arg("30").spawn(),
            &mut |_| panic!("no turn without a socket"),
            std::time::Duration::from_millis(300),
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

    /// The release marker binds writer to reader: the name every release build
    /// is given is the name the binary's `BUILD_STAMP.release_build` reads.
    /// Negative control: the same check over a name the binary does not read
    /// fails, so a marker renamed on either side fails here.
    #[test]
    fn the_release_marker_is_the_name_the_app_reads() {
        let main =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../aterm/src/main.rs"))
                .expect("read the binary's main.rs");
        let reads = |name: &str| {
            main.contains(&format!(
                "release_build: option_env!(\"{name}\").is_some(),"
            ))
        };
        assert!(reads(super::RELEASE_BUILD_MARKER));
        assert!(!reads("ATERM_NO_SUCH_MARKER"));
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

    #[test]
    fn rustup_shim_resolver_prefers_standard_and_builds_a_closed_path() {
        let scratch = PrivateSliceDir::create().unwrap();
        let home = scratch.0.join("home");
        let standard = home.join(".cargo/bin");
        let fallback = scratch.0.join("fallback/bin");
        create_test_rustup_shims(&standard);
        create_test_rustup_shims(&fallback);

        let resolved =
            resolve_release_rustup_shim_dir(&home, &fallback, current_release_uid().unwrap())
                .unwrap();
        assert_eq!(resolved, standard.canonicalize().unwrap());
        assert_eq!(
            release_tool_path(&resolved).unwrap(),
            std::ffi::OsString::from(format!("{}:{RELEASE_SYSTEM_PATH}", resolved.display()))
        );
    }

    #[test]
    fn rustup_shim_resolver_canonicalizes_the_homebrew_fallback() {
        let scratch = PrivateSliceDir::create().unwrap();
        let home = scratch.0.join("home");
        let cellar = scratch.0.join("Cellar/rustup/1.0");
        let opt = scratch.0.join("opt");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&opt).unwrap();
        create_test_rustup_shims(&cellar.join("bin"));
        symlink(&cellar, opt.join("rustup")).unwrap();

        let resolved = resolve_release_rustup_shim_dir(
            &home,
            &opt.join("rustup/bin"),
            current_release_uid().unwrap(),
        )
        .unwrap();
        assert_eq!(resolved, cellar.join("bin").canonicalize().unwrap());
    }

    #[test]
    fn rustup_shim_resolver_refuses_an_unsafe_standard_instead_of_falling_back() {
        let scratch = PrivateSliceDir::create().unwrap();
        let home = scratch.0.join("home");
        let standard = home.join(".cargo/bin");
        let fallback = scratch.0.join("fallback/bin");
        create_test_rustup_shims(&standard);
        create_test_rustup_shims(&fallback);
        std::fs::set_permissions(&standard, std::fs::Permissions::from_mode(0o770)).unwrap();

        let error =
            resolve_release_rustup_shim_dir(&home, &fallback, current_release_uid().unwrap())
                .unwrap_err();
        assert!(error.contains("group/other-writable"), "{error}");
    }

    #[test]
    fn rustup_shim_resolver_requires_owned_real_executables() {
        let scratch = PrivateSliceDir::create().unwrap();
        let home = scratch.0.join("home");
        let standard = home.join(".cargo/bin");
        let fallback = scratch.0.join("fallback/bin");
        create_test_rustup_shims(&standard);
        create_test_rustup_shims(&fallback);
        std::fs::remove_file(standard.join("cargo")).unwrap();
        symlink("rustup", standard.join("cargo")).unwrap();

        let error =
            resolve_release_rustup_shim_dir(&home, &fallback, current_release_uid().unwrap())
                .unwrap_err();
        assert!(error.contains("not a real regular file"), "{error}");
    }

    #[test]
    fn both_release_arches_use_one_locked_offline_dependency_source() {
        let workspace_config =
            std::path::Path::new("/c-release.noindex/snapshot/src/.cargo/config.toml");
        let source_config = std::path::Path::new("/c-release.noindex/snapshot/.cargo/config.toml");
        let manifest = std::path::Path::new("/c-release.noindex/snapshot/src/Cargo.toml");
        let native = cargo_build_args(workspace_config, source_config, manifest, "aterm", None);
        let compat = cargo_build_args(
            workspace_config,
            source_config,
            manifest,
            "aterm",
            Some("x86_64-apple-darwin"),
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
            assert!(args.contains(&"--locked".to_owned()));
            assert!(args.contains(&"--offline".to_owned()));
        }
        assert_eq!(native.first().map(String::as_str), Some("--unverified"));
        assert!(!compat.contains(&"--unverified".to_owned()));
        assert!(
            compat
                .windows(2)
                .any(|pair| pair == ["--target", "x86_64-apple-darwin"])
        );
    }

    /// THE CUTTER NEVER COMPILES A DEVELOPMENT SEAM. Every
    /// `aterm_types::dev_seam!` read — `ATPKG_REGISTRY`, `ATERM_UPDATE_ROOT`, the
    /// handoff deadlines, the `ATERM_DEBUG_*` seams — and every development setting
    /// (`[update]` owner/repo/require_team_id, `[packages]` prefix/account/repo links)
    /// compiles only under `cfg(any(debug_assertions, feature = "dev-seams"))`. A
    /// release build has no `debug_assertions`, so the one way a shipped binary could
    /// carry a seam is the feature — and this pins the three ways it could arrive: the
    /// argv (no `--features`/`-F`/`--all-features`, nothing naming the feature), the
    /// build's environment (cleared, so no inherited `RUSTFLAGS` can add
    /// `--cfg feature="dev-seams"`), and the manifests (no crate enables it by default
    /// or through a dependency — only the `dev-seams` feature lists forward it).
    #[test]
    fn the_release_build_enables_no_development_seam() {
        let config = std::path::Path::new("/private/take/.cargo/config.toml");
        let manifest = std::path::Path::new("/private/take/Cargo.toml");
        for target in [None, Some("x86_64-apple-darwin")] {
            let args: Vec<String> = cargo_build_args(config, config, manifest, "aterm", target)
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
        let src = include_str!("buildplan.rs");
        let body = &src[src.find("\nfn build_one(").expect("build_one")..];
        let body = &body[..body[1..].find("\nfn ").map_or(body.len(), |i| i + 1)];
        let clear = body
            .find("cmd.env_clear();")
            .expect("the build env is cleared");
        let args = body.find("cmd.args(cargo_build_args(").expect("the argv");
        assert!(
            args < clear,
            "the environment is cleared after the argv is set"
        );
        assert!(
            !body.contains(".env(\"RUSTFLAGS\"")
                && !body.contains(".env(\"CARGO_ENCODED_RUSTFLAGS\"")
                && !body.contains(".env(\"CARGO_BUILD_RUSTFLAGS\""),
            "no rustflags are handed to the release build"
        );

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let mut manifests = vec![root.join("Cargo.toml")];
        for entry in std::fs::read_dir(root.join("crates"))
            .expect("crates/")
            .flatten()
        {
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
            // Every dependency table, however it is spelled: a `features = [..]`
            // that names the seam compiles it into whatever depends on it.
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
        assert!(
            forwarders >= 6,
            "the seam feature is declared by every crate that reads a seam; found {forwarders}"
        );

        // DEBUG ASSERTIONS TURN THE SEAMS ON TOO: `dev_seam!` compiles
        // under `cfg(any(debug_assertions, feature = "dev-seams"))`, so a
        // `debug-assertions = true` in the release profile — added for overflow checks,
        // say — or a `-C debug-assertions` / `--cfg feature="dev-seams"` in the config the
        // cutter passes (`--config <take>/.cargo/config.toml`) would make every shipped
        // binary honour ATPKG_REGISTRY, ATERM_UPDATE_ROOT and the `[packages]`/`[update]`
        // development settings, with every pin above still green.
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
        // Non-vacuity: the walk sees a key set anywhere in a table, and a flag string.
        let armed: aterm_toml::Table =
            aterm_toml::from_str("[package.aterm-gui]\ndebug-assertions = true\n").unwrap();
        assert!(
            std::panic::catch_unwind(|| assert_no_debug_assertions("fixture", &armed)).is_err()
        );
        let flagged: aterm_toml::Table =
            aterm_toml::from_str("[build]\nrustflags = [\"-C\", \"debug-assertions=on\"]\n")
                .unwrap();
        assert!(
            std::panic::catch_unwind(|| assert_no_debug_assertions("fixture", &flagged)).is_err()
        );
    }

    /// No table under `table` turns debug assertions (and so the development seams) on:
    /// no `debug-assertions = true` at any depth (a profile, a `package.*` override, a
    /// `build-override`), and no string (a rustflags entry) naming `debug-assertions`,
    /// `debug_assertions` or `dev-seams`.
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

    /// Everything a verified take built leaves the clone's shared stage for `dist/`:
    /// the stripped binary the bundle copies, too, so a later take of the clone (which
    /// recreates the stage) cannot pull it out from under the cut.
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
        assert!(
            !out.join(".metadata_never_index").exists(),
            "no build-output marker: inert for Spotlight, and nothing reads it any more"
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

        assert!(error.contains("not a real directory"), "{error}");
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

    /// Minimal structurally valid arm64 thin Mach-O.  `declared_size` is kept
    /// separate from the data length so negative tests can exercise exact-size
    /// and bounds checks independently.
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
        // The dev-build spelling of the same workspace version is NOT the
        // release version: only DEV == 0 ships.
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
        // The S12 which-copy lines may follow the identity line — and only those.
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
        // The origin line may follow the identity line, exactly as shared, then
        // the which-copy lines; a paraphrase of it is still "anything else".
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
