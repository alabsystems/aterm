// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build plan (release spec §6 `buildplan.rs`): per-arch cargo builds of
//! aterm-gui + atpkg + aterm-ctl + aterm-cli with `SOURCE_DATE_EPOCH=n` as the SOLE
//! build-number conduit (zero build.rs changes — spec decision 11), `lipo` to
//! universal (single-arch pass-through under `--arm64-only`), then dSYM
//! (success judged by DWARF file existence — inherited dsymutil exit-code
//! caveat — plus UUID match), `strip -x` on shipped copies, and the dSYM zip.
//!
//! TWO compiler lanes and no third (owner decision, 2026-07): the native aarch64
//! slice on Trust (the repo's rust-toolchain.toml pin plus .cargo/config.toml's
//! documented verification opt-out) and the x86_64-apple-darwin compat slice on
//! upstream stable. Both architectures build from one read-only source take and one
//! lock-checksummed, offline dependency bundle; only their compiler differs — which
//! is why "ONE compiler lane", as this paragraph used to open, described the opposite
//! of the sentence that followed it. The ONLY toolchain override is
//! `RUSTUP_TOOLCHAIN=stable` on the compat slice, set in `run_cargo` AFTER inherited
//! RUSTC / RUSTC_BOOTSTRAP / RUSTFLAGS / RUSTUP_TOOLCHAIN are scrubbed on both lanes:
//! no `RUSTC=…` pin, no RUSTFLAGS surgery, no `--no-trust` flag. The native binary
//! must still self-report `+t` (see [`run`]).
//!
//! The ONE exception: the x86_64-apple-darwin compat slice of the universal
//! binary rides upstream stable via `RUSTUP_TOOLCHAIN=stable`. The reason is
//! NOT that Trust lacks an x86_64 std — it has one, and six ALab programs ship
//! x86_64 artifacts built with it. What a CROSS-HOST Trust sysroot lacks is
//! rustc_private, so an out-of-tree rustc-driver tool cannot link against it;
//! that is a narrower gap than "no std", and it is why the rustc coherence
//! group is still aarch64-only while the plain programs are not. The compat
//! slice rides stable because this lane wants no Trust-specific state on it at
//! all. That pin lives HERE and nowhere else, and the lane scrubs
//! inherited RUSTC/RUSTFLAGS state so stale shell exports cannot steer it.
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
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// THE shipped binary: `(cargo package, cargo bin name, basename it ships
/// under)` — all three are `aterm`. One binary is the whole surface: the
/// window (a no-TTY/Finder launch), the transparent session (a TTY launch),
/// and every verb (`aterm ctl/pkg/fleet/drive/help`) in-process. The bundle
/// adds argv0 compat SYMLINKS (aterm-ctl, atpkg, aterm-fleet, aterm-drive,
/// aterm-gui, aterm-cli) pointing at it — see `bundle::assemble` — so
/// pre-one-binary scripts, installs, and Help examples keep resolving.
const PACKAGES: [(&str, &str, &str); 1] = [("aterm", "aterm", "aterm")];

const ARM64: &str = "aarch64-apple-darwin";
const X86_64: &str = "x86_64-apple-darwin";
const LIPO_ARM64: &str = "arm64";
const LIPO_X86_64: &str = "x86_64";

/// Everything [`run`] needs, resolved by the caller (cli/gates own flag
/// parsing and the ledger claim; this module only builds).
pub struct BuildPlan {
    /// Workspace root — release orchestration runs here; Cargo builds receive
    /// its sealed snapshot manifest explicitly from an inert root cwd.
    pub repo_root: PathBuf,
    /// `dist/` — receives the dSYM + dSYM zip (the .app lands there later,
    /// via `bundle::assemble`).
    pub out_dir: PathBuf,
    /// The claimed ledger number `n`, exported as `SOURCE_DATE_EPOCH` to every
    /// cargo child (→ ATERM_BUILD_NUMBER + ATERM_BUILD_TIME via the untouched
    /// build.rs — spec decision 11).
    pub build_number: u64,
    /// Release version ("0.2.0") — names the dSYM zip `aterm-0.2.0-dSYM.zip`.
    pub short_version: String,
    /// `--arm64-only`: skip the x86_64 slice (single-arch pass-through
    /// instead of lipo). The universal build is the default (spec decision 18).
    pub arm64_only: bool,
    /// Exact lowercase SHA-256 fingerprint that every independently-built and
    /// final shipped architecture slice must embed for its compiled updater
    /// key. `None` exists only for unsigned development/rehearsal fixtures.
    pub expected_update_pin_sha256: Option<String>,
}

/// What [`run`] produced: the stripped, ship-ready universal binaries plus the
/// dSYM artifacts. Paths are the copies `bundle::assemble` should place in
/// `Contents/MacOS` verbatim (already `strip -x`ed; symbols live in the dSYM,
/// matched by the Mach-O UUID, which strip preserves).
pub struct BuildOutput {
    /// Stripped universal `aterm` (the shipped GUI binary).
    pub aterm: PathBuf,
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
    /// `dist/aterm-<ver>-dSYM.zip` — the archive attached to the release.
    pub dsym_zip: Option<PathBuf>,
}

#[cfg(unix)]
fn current_release_uid() -> Result<u32, String> {
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
fn current_release_uid() -> Result<u32, String> {
    Err("release target privacy requires Unix ownership and mode semantics".into())
}

/// Open one release-owned directory without accepting a symlink substitution.
///
/// The first metadata read rejects a static symlink. Opening and comparing the
/// device/inode pair before any chmod closes the check/use gap: if the path was
/// swapped while it was opened, no referent is modified. All later permission
/// changes apply to the opened directory descriptor, and the final path check
/// proves the same directory remains published at `path`.
#[cfg(unix)]
fn open_release_directory(
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
fn open_release_directory(
    _path: &Path,
    _expected_uid: u32,
    _private: bool,
    _what: &str,
) -> Result<File, String> {
    Err("release target privacy requires Unix ownership and mode semantics".into())
}

#[cfg(unix)]
fn open_release_file(
    path: &Path,
    expected_uid: u32,
    expected_mode: u32,
    what: &str,
) -> Result<File, String> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let before = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {what} {}: {error}", path.display()))?;
    if !before.file_type().is_file() {
        return Err(format!("{what} is not a regular file: {}", path.display()));
    }
    if before.uid() != expected_uid {
        return Err(format!(
            "{what} is owned by uid {}, expected {expected_uid}: {}",
            before.uid(),
            path.display()
        ));
    }
    let file =
        File::open(path).map_err(|error| format!("open {what} {}: {error}", path.display()))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("inspect opened {what} {}: {error}", path.display()))?;
    if !opened.file_type().is_file()
        || opened.uid() != expected_uid
        || (opened.dev(), opened.ino()) != (before.dev(), before.ino())
    {
        return Err(format!(
            "{what} changed while it was opened: {}",
            path.display()
        ));
    }
    let mut permissions = opened.permissions();
    permissions.set_mode(expected_mode);
    file.set_permissions(permissions)
        .map_err(|error| format!("make {what} private {}: {error}", path.display()))?;
    let after = file
        .metadata()
        .map_err(|error| format!("reinspect opened {what} {}: {error}", path.display()))?;
    let published = std::fs::symlink_metadata(path)
        .map_err(|error| format!("reinspect {what} {}: {error}", path.display()))?;
    if !published.file_type().is_file()
        || published.uid() != expected_uid
        || (published.dev(), published.ino()) != (after.dev(), after.ino())
        || published.mode() & 0o777 != expected_mode
    {
        return Err(format!(
            "{what} is not the opened private file: {}",
            path.display()
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_release_file(
    _path: &Path,
    _expected_uid: u32,
    _expected_mode: u32,
    _what: &str,
) -> Result<File, String> {
    Err("release target privacy requires Unix ownership and mode semantics".into())
}

fn create_private_release_directory(
    path: &Path,
    expected_uid: u32,
    what: &str,
) -> Result<(), String> {
    std::fs::create_dir(path)
        .map_err(|error| format!("create {what} {}: {error}", path.display()))?;
    open_release_directory(path, expected_uid, true, what).map(drop)
}

fn prepare_release_target_parent(repo_root: &Path) -> Result<(PathBuf, u32), String> {
    let expected_uid = current_release_uid()?;
    let target_root = repo_root.join("target");
    match std::fs::create_dir(&target_root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "create release target root {}: {error}",
                target_root.display()
            ));
        }
    }
    // Reject an ignored `target` symlink before constructing or collecting any
    // child beneath it. Otherwise a clean checkout could redirect residue GC.
    drop(open_release_directory(
        &target_root,
        expected_uid,
        false,
        "release target root",
    )?);

    let parent = target_root.join("release-takes");
    match std::fs::create_dir(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "create release target parent {}: {error}",
                parent.display()
            ));
        }
    }
    drop(open_release_directory(
        &parent,
        expected_uid,
        true,
        "release target parent",
    )?);
    Ok((parent, expected_uid))
}

/// One immutable source/dependency take shared by both release architectures.
struct SealedCargoTake {
    repo_root: PathBuf,
    source_fingerprint: String,
    source_root: PathBuf,
    workspace_config: PathBuf,
    cargo_config: PathBuf,
    target_dir: PathBuf,
    cargo_home: PathBuf,
    temporary_dir: PathBuf,
    lease_pid: u32,
    lease_token: String,
    release_uid: u32,
    target_cleaned: bool,
    lease_cleaned: bool,
}

impl SealedCargoTake {
    fn acquire(repo_root: &Path) -> Result<Self, String> {
        let fingerprinter = repo_root.join("tools/artifact_source_fingerprint.py");
        let snapshotter = repo_root.join("tools/proof_snapshot.py");
        for tool in [&fingerprinter, &snapshotter] {
            let metadata = std::fs::symlink_metadata(tool).map_err(|error| {
                format!("inspect release proof tool {}: {error}", tool.display())
            })?;
            if !metadata.file_type().is_file() {
                return Err(format!(
                    "release proof tool is not a regular file: {}",
                    tool.display()
                ));
            }
        }
        let mut fingerprint_command = release_proof_python(repo_root, &fingerprinter)?;
        fingerprint_command
            .arg("--root")
            .arg(repo_root)
            .args(["--package", "aterm", "--source-only"])
            .current_dir(repo_root);
        let fingerprint_output = checked_output(
            &mut fingerprint_command,
            "derive release source fingerprint",
        )?;
        let fingerprint =
            one_output_line(&fingerprint_output, "release source fingerprint")?.to_owned();
        if fingerprint.len() != 64
            || !fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err("release source fingerprint is not canonical SHA-256".into());
        }
        let lease_pid = std::process::id();
        let lease_token = fresh_lease_token()?;
        let mut snapshot_command = release_proof_python(repo_root, &snapshotter)?;
        snapshot_command
            .arg("--root")
            .arg(repo_root)
            .args(["--package", "aterm", "--fingerprint"])
            .arg(&fingerprint)
            .arg("--source-fingerprint")
            .arg(&fingerprint)
            .arg("--lease-pid")
            .arg(lease_pid.to_string())
            .arg("--lease-token")
            .arg(&lease_token)
            .current_dir(repo_root);
        let snapshot_output =
            checked_output(&mut snapshot_command, "materialize release source take")?;
        let acquired = (|| {
            let source_root =
                PathBuf::from(one_output_line(&snapshot_output, "release source take")?.to_owned());
            if !source_root.is_absolute() {
                return Err("release source take path is not absolute".into());
            }
            let source_root = source_root
                .canonicalize()
                .map_err(|error| format!("canonicalize release source take: {error}"))?;
            let workspace_config = source_root.join(".cargo/config.toml");
            let cargo_config = source_root.join(".aterm-proof-registry/.cargo/config.toml");
            for (kind, config) in [
                ("workspace", &workspace_config),
                ("sealed source", &cargo_config),
            ] {
                let metadata = std::fs::symlink_metadata(config)
                    .map_err(|error| format!("inspect {kind} Cargo config: {error}"))?;
                if !metadata.file_type().is_file() {
                    return Err(format!("{kind} Cargo config is not a regular file"));
                }
            }
            let (target_parent, release_uid) = prepare_release_target_parent(repo_root)?;
            cleanup_release_target_residue(&target_parent)?;
            let take_name = source_root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("release source take has no UTF-8 identity")?;
            let target_dir = target_parent.join(format!("{take_name}-{lease_pid}-{lease_token}"));
            create_private_release_directory(&target_dir, release_uid, "fresh release target")?;
            let cargo_home = target_dir.join("cargo-home");
            let temporary_dir = target_dir.join("tmp");
            let setup = (|| {
                let start = process_start_identity(lease_pid)?
                    .ok_or("release process disappeared while creating its target")?;
                write_release_target_owner(&target_dir, lease_pid, &start, &lease_token)?;
                create_private_release_directory(&cargo_home, release_uid, "release Cargo home")?;
                create_private_release_directory(
                    &temporary_dir,
                    release_uid,
                    "release temporary directory",
                )?;
                Ok(())
            })();
            if let Err(error) = setup {
                return match std::fs::remove_dir_all(&target_dir) {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(format!(
                        "{error}; additionally, remove partial target: {cleanup}"
                    )),
                };
            }
            Ok(Self {
                repo_root: repo_root.to_path_buf(),
                source_fingerprint: fingerprint.clone(),
                source_root,
                workspace_config,
                cargo_config,
                target_dir,
                cargo_home,
                temporary_dir,
                lease_pid,
                lease_token: lease_token.clone(),
                release_uid,
                target_cleaned: false,
                lease_cleaned: false,
            })
        })();
        match acquired {
            Ok(take) => Ok(take),
            Err(error) => match release_snapshot_lease(repo_root, lease_pid, &lease_token) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; additionally, {cleanup}")),
            },
        }
    }

    fn release(&mut self) -> Result<(), String> {
        if self.target_cleaned && self.lease_cleaned {
            return Ok(());
        }
        let mut errors = Vec::new();
        if !self.target_cleaned {
            match remove_owned_release_target(
                &self.target_dir,
                self.release_uid,
                self.lease_pid,
                &self.lease_token,
            ) {
                Ok(()) => self.target_cleaned = true,
                Err(error) => errors.push(format!(
                    "remove private release target {}: {error}",
                    self.target_dir.display()
                )),
            }
        }
        if !self.lease_cleaned {
            match release_snapshot_lease(&self.repo_root, self.lease_pid, &self.lease_token) {
                Ok(()) => self.lease_cleaned = true,
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; additionally, "))
        }
    }

    fn verify(&self) -> Result<(), String> {
        let script = self.repo_root.join("tools/proof_snapshot.py");
        let mut command = release_proof_python(&self.repo_root, &script)?;
        command
            .arg("--root")
            .arg(&self.repo_root)
            .args(["--package", "aterm", "--source-fingerprint"])
            .arg(&self.source_fingerprint)
            .arg("--verify-registry-archives")
            .arg("--verify-snapshot")
            .arg(&self.source_root);
        checked_output(
            &mut command,
            "verify sealed release source take after build",
        )?;
        Ok(())
    }
}

fn cleanup_release_target_residue(parent: &Path) -> Result<(), String> {
    cleanup_release_target_residue_with(parent, process_start_identity)
}

fn cleanup_release_target_residue_with(
    parent: &Path,
    process_start: impl Fn(u32) -> Result<Option<String>, String>,
) -> Result<(), String> {
    let expected_uid = current_release_uid()?;
    cleanup_release_target_residue_with_uid(parent, expected_uid, process_start)
}

fn cleanup_release_target_residue_with_uid(
    parent: &Path,
    expected_uid: u32,
    process_start: impl Fn(u32) -> Result<Option<String>, String>,
) -> Result<(), String> {
    // Validate the base itself before `read_dir`: an ignored symlink here must
    // never redirect the collector outside this checkout's owned target tree.
    drop(open_release_directory(
        parent,
        expected_uid,
        true,
        "release target parent",
    )?);
    for entry in std::fs::read_dir(parent)
        .map_err(|error| format!("scan release target residue: {error}"))?
    {
        let entry = entry.map_err(|error| format!("read release target residue: {error}"))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or("release target residue has a non-UTF-8 name")?;
        let parts = name.split('-').collect::<Vec<_>>();
        let (pid, token) = match parts.as_slice() {
            [take, pid] if is_lower_hex(take, 64) => (pid.parse::<u32>().ok(), None),
            [take, pid, token] if is_lower_hex(take, 64) && is_lower_hex(token, 64) => {
                (pid.parse::<u32>().ok(), Some(*token))
            }
            _ => continue,
        };
        let Some(pid) = pid.filter(|pid| *pid > 0) else {
            continue;
        };
        let path = entry.path();
        drop(open_release_directory(
            &path,
            expected_uid,
            true,
            "release target residue",
        )?);
        let current_start = process_start(pid)?;
        let owner = path.join(".owner");
        let stale = match std::fs::symlink_metadata(&owner) {
            Ok(owner_metadata) => {
                if !owner_metadata.file_type().is_file() {
                    return Err(format!(
                        "release target owner is not a regular file: {name}"
                    ));
                }
                let (owner_pid, owner_start, owner_token) =
                    read_release_target_owner(&owner, expected_uid)?;
                if owner_pid != pid || token != Some(owner_token.as_str()) {
                    return Err(format!(
                        "release target owner disagrees with its name: {name}"
                    ));
                }
                current_start.as_deref() != Some(owner_start.as_str())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => current_start.is_none(),
            Err(error) => return Err(format!("inspect release target owner {name}: {error}")),
        };
        if stale {
            std::fs::remove_dir_all(&path).map_err(|error| {
                format!(
                    "remove abandoned release target {}: {error}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn prepare_release_product_parent(repo_root: &Path, expected_uid: u32) -> Result<PathBuf, String> {
    let target_root = repo_root.join("target");
    drop(open_release_directory(
        &target_root,
        expected_uid,
        false,
        "release target root",
    )?);
    let parent = target_root.join("release-products");
    match std::fs::create_dir(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "create release product parent {}: {error}",
                parent.display()
            ));
        }
    }
    drop(open_release_directory(
        &parent,
        expected_uid,
        true,
        "release product parent",
    )?);
    cleanup_release_target_residue_with_uid(&parent, expected_uid, process_start_identity)?;
    Ok(parent)
}

fn write_release_target_owner(
    target: &Path,
    pid: u32,
    start: &str,
    token: &str,
) -> Result<(), String> {
    let expected_uid = current_release_uid()?;
    drop(open_release_directory(
        target,
        expected_uid,
        true,
        "release target",
    )?);
    let temporary = target.join(".owner.pending");
    let owner = target.join(".owner");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("create release target owner: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("make release target owner private: {error}"))?;
    }
    write!(file, "version=1\npid={pid}\nstart={start}\ntoken={token}\n")
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("persist release target owner: {error}"))?;
    drop(file);
    std::fs::rename(&temporary, &owner)
        .map_err(|error| format!("publish release target owner: {error}"))?;
    drop(open_release_file(
        &owner,
        expected_uid,
        0o600,
        "release target owner",
    )?);
    File::open(target)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("persist release target owner directory: {error}"))
}

fn read_release_target_owner(
    path: &Path,
    expected_uid: u32,
) -> Result<(u32, String, String), String> {
    let mut file = open_release_file(path, expected_uid, 0o600, "release target owner")?;
    let length = file
        .metadata()
        .map_err(|error| format!("inspect release target owner {}: {error}", path.display()))?
        .len();
    if length > 512 {
        return Err(format!(
            "release target owner is oversized: {}",
            path.display()
        ));
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|error| format!("read release target owner {}: {error}", path.display()))?;
    let lines = contents.lines().collect::<Vec<_>>();
    if lines.len() != 4 || lines[0] != "version=1" {
        return Err(format!(
            "malformed release target owner: {}",
            path.display()
        ));
    }
    let pid = lines[1]
        .strip_prefix("pid=")
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("malformed release target owner PID: {}", path.display()))?;
    let start = lines[2]
        .strip_prefix("start=")
        .filter(|value| !value.is_empty() && value.len() <= 80 && value.is_ascii())
        .ok_or_else(|| format!("malformed release target owner start: {}", path.display()))?;
    let token = lines[3]
        .strip_prefix("token=")
        .filter(|value| is_lower_hex(value, 64))
        .ok_or_else(|| format!("malformed release target owner token: {}", path.display()))?;
    Ok((pid, start.to_owned(), token.to_owned()))
}

fn remove_owned_release_target(
    target: &Path,
    expected_uid: u32,
    expected_pid: u32,
    expected_token: &str,
) -> Result<(), String> {
    match std::fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "inspect release target {}: {error}",
                target.display()
            ));
        }
        Ok(_) => {}
    }
    drop(open_release_directory(
        target,
        expected_uid,
        true,
        "release target",
    )?);
    let (pid, _, token) = read_release_target_owner(&target.join(".owner"), expected_uid)?;
    if pid != expected_pid || token != expected_token {
        return Err("release target owner does not match this source-take lease".into());
    }
    std::fs::remove_dir_all(target)
        .map_err(|error| format!("remove release target {}: {error}", target.display()))
}

fn process_start_identity(pid: u32) -> Result<Option<String>, String> {
    let output = Command::new("/bin/ps")
        .env_clear()
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .map_err(|error| format!("inspect release target process {pid}: {error}"))?;
    if output.status.success() {
        let raw = std::str::from_utf8(&output.stdout)
            .map_err(|error| format!("release target process identity is not UTF-8: {error}"))?;
        let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() || normalized.len() > 80 || !normalized.is_ascii() {
            return Err(format!(
                "release target process {pid} has a malformed identity"
            ));
        }
        return Ok(Some(normalized));
    }
    let probe = Command::new("/bin/kill")
        .env_clear()
        .env("LC_ALL", "C")
        .args(["-0", &pid.to_string()])
        .output()
        .map_err(|error| format!("probe release target process {pid}: {error}"))?;
    if probe.status.success() {
        return Err(format!(
            "live release target process {pid} could not be identified"
        ));
    }
    let diagnostic = String::from_utf8_lossy(&probe.stderr);
    if diagnostic.contains("No such process") {
        Ok(None)
    } else {
        Err(format!(
            "release target process {pid} absence was not proven: {}",
            diagnostic.trim()
        ))
    }
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn fresh_lease_token() -> Result<String, String> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("derive release lease time: {error}"))?
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(format!(
        "{nanos:032x}{:08x}{sequence:024x}",
        std::process::id()
    ))
}

fn release_snapshot_lease(
    repo_root: &Path,
    lease_pid: u32,
    lease_token: &str,
) -> Result<(), String> {
    let script = repo_root.join("tools/proof_snapshot.py");
    let mut command = release_proof_python(repo_root, &script)?;
    command
        .arg("--root")
        .arg(repo_root)
        .arg("--release-pid")
        .arg(lease_pid.to_string())
        .arg("--lease-token")
        .arg(lease_token)
        .current_dir(repo_root);
    checked_output(&mut command, "release sealed Cargo take lease")?;
    Ok(())
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

fn release_proof_python(repo_root: &Path, script: &Path) -> Result<Command, String> {
    let home = std::env::var_os("HOME").ok_or("release proof tooling requires HOME")?;
    let cargo_home = std::env::var_os("CARGO_HOME")
        .unwrap_or_else(|| Path::new(&home).join(".cargo").into_os_string());
    let rustup_home = release_rustup_home(&home)?;
    let shim_dir = release_rustup_shim_dir(&home)?;
    let path = release_tool_path(&shim_dir)?;
    let mut command = Command::new("/usr/bin/python3");
    command
        .args([
            "-I",
            "-S",
            "-c",
            "import runpy,sys; sys.path.insert(0,sys.argv[1]); sys.argv=sys.argv[2:]; runpy.run_path(sys.argv[0],run_name='__main__')",
        ])
        .arg(
            script
                .parent()
                .ok_or("release proof script has no parent directory")?,
        )
        .arg(script)
        .env_clear()
        .env("HOME", home)
        .env("PATH", path)
        .env("TMPDIR", "/tmp")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("PYTHONHASHSEED", "0")
        .env("PYTHONNOUSERSITE", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("CARGO_HOME", cargo_home)
        .env("RUSTUP_HOME", rustup_home)
        .current_dir(repo_root);
    Ok(command)
}

fn release_rustup_home(home: &std::ffi::OsStr) -> Result<OsString, String> {
    let value = std::env::var_os("RUSTUP_HOME")
        .unwrap_or_else(|| Path::new(home).join(".rustup").into_os_string());
    if !Path::new(&value).is_absolute() {
        return Err("release RUSTUP_HOME must be absolute".into());
    }
    Ok(value)
}

impl Drop for SealedCargoTake {
    fn drop(&mut self) {
        let _ = self.release();
    }
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

fn one_output_line<'a>(output: &'a Output, what: &str) -> Result<&'a str, String> {
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|error| format!("{what} is not UTF-8: {error}"))?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.is_empty() || line.contains(['\n', '\r']) {
        return Err(format!("{what} did not emit exactly one line"));
    }
    Ok(line)
}

/// Run the whole build phase: per-arch cargo builds → lipo → dsymutil →
/// strip → dSYM zip. Returns the ship-ready binaries.
pub fn run(plan: &BuildPlan) -> Result<BuildOutput, String> {
    let mut take = SealedCargoTake::acquire(&plan.repo_root)?;
    let staged_out = take.target_dir.join("dist");
    let built = run_with_take(plan, &take, &staged_out);
    let verified = take.verify();
    let published = match (built, verified) {
        (Ok(output), Ok(())) => publish_verified_output(&take, output, &plan.out_dir),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(build), Err(verify)) => Err(format!("{build}; additionally, {verify}")),
    };
    let released = take.release();
    let mut errors = Vec::new();
    if let Err(error) = &published {
        errors.push(error.clone());
    }
    if let Err(error) = released {
        errors.push(error);
    }
    if errors.is_empty() {
        published
    } else {
        Err(errors.join("; additionally, "))
    }
}

/// Move the verified release products out of the disposable Cargo target.
///
/// The binary is published first into a unique, private, owner-recorded
/// directory. The hard link is one atomic filesystem operation and binds the
/// handoff path to the exact inode that passed every post-strip gate. Only
/// after that durable handoff succeeds are symbols moved into `dist/`.
fn publish_verified_output(
    take: &SealedCargoTake,
    mut output: BuildOutput,
    out_dir: &Path,
) -> Result<BuildOutput, String> {
    let take_identity = take
        .source_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("release source take has no UTF-8 identity")?;
    output.aterm = publish_verified_binary(
        &take.repo_root,
        &output.aterm,
        take_identity,
        take.lease_pid,
        &take.lease_token,
        take.release_uid,
    )?;
    publish_verified_symbols(output, out_dir, take.release_uid)
}

fn publish_verified_binary(
    repo_root: &Path,
    staged: &Path,
    take_identity: &str,
    lease_pid: u32,
    lease_token: &str,
    expected_uid: u32,
) -> Result<PathBuf, String> {
    if !is_lower_hex(take_identity, 64) {
        return Err("release source take identity is not canonical SHA-256".into());
    }
    if lease_pid == 0 || !is_lower_hex(lease_token, 64) {
        return Err("release product lease identity is malformed".into());
    }

    let parent = prepare_release_product_parent(repo_root, expected_uid)?;
    let product = parent.join(format!("{take_identity}-{lease_pid}-{lease_token}"));
    create_private_release_directory(&product, expected_uid, "release product")?;
    let start = match process_start_identity(lease_pid) {
        Ok(Some(start)) => start,
        Ok(None) => {
            let error = "release process disappeared while publishing its binary".to_owned();
            return match std::fs::remove_dir_all(&product) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; additionally, remove product: {cleanup}")),
            };
        }
        Err(error) => {
            return match std::fs::remove_dir_all(&product) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; additionally, remove product: {cleanup}")),
            };
        }
    };
    if let Err(error) = write_release_target_owner(&product, lease_pid, &start, lease_token) {
        return match std::fs::remove_dir_all(&product) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(format!("{error}; additionally, remove product: {cleanup}")),
        };
    }

    let publish = (|| {
        let staged_file = open_release_file(
            staged,
            expected_uid,
            0o500,
            "verified staged release binary",
        )?;
        staged_file
            .sync_all()
            .map_err(|error| format!("persist verified staged release binary: {error}"))?;

        let published = product.join("aterm");
        std::fs::hard_link(staged, &published).map_err(|error| {
            format!(
                "atomically publish verified release binary {} -> {}: {error}",
                staged.display(),
                published.display()
            )
        })?;
        let published_file =
            open_release_file(&published, expected_uid, 0o500, "published release binary")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            let staged_metadata = staged_file
                .metadata()
                .map_err(|error| format!("reinspect verified staged release binary: {error}"))?;
            let published_metadata = published_file
                .metadata()
                .map_err(|error| format!("reinspect published release binary: {error}"))?;
            if (staged_metadata.dev(), staged_metadata.ino())
                != (published_metadata.dev(), published_metadata.ino())
            {
                return Err("published release binary is not the verified staged inode".to_owned());
            }
        }
        published_file
            .sync_all()
            .map_err(|error| format!("persist published release binary: {error}"))?;
        File::open(&product)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("persist release product directory: {error}"))?;
        File::open(&parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("persist release product parent: {error}"))?;
        Ok(published)
    })();

    match publish {
        Ok(published) => Ok(published),
        Err(error) => {
            match remove_owned_release_target(&product, expected_uid, lease_pid, lease_token) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!("{error}; additionally, remove product: {cleanup}")),
            }
        }
    }
}

fn publish_verified_symbols(
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
    // `dist` is ignored and therefore can pre-exist as an attacker-controlled
    // symlink. Validate and tighten it before any marker write or destructive
    // replacement below can be redirected outside this checkout.
    drop(open_release_directory(
        out_dir,
        expected_uid,
        true,
        "release output directory",
    )?);
    // The build-output sentinel atpkg reads (`cli.rs::is_build_output_bundle`), NOT a
    // Spotlight exclusion: a `.metadata_never_index` file in a subdirectory is INERT,
    // measured 2026-09-02 (crates/atpkg/src/noindex.rs). The only mechanism measured to
    // exclude a subtree is a name ending `.noindex`, which `dist/` does not have.
    std::fs::write(out_dir.join(".metadata_never_index"), "")
        .map_err(|error| format!("mark {} build output: {error}", out_dir.display()))?;
    if let Some(staged) = output.dsym.take() {
        let published = out_dir.join("aterm.dSYM");
        match std::fs::remove_dir_all(&published) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("replace {}: {error}", published.display())),
        }
        std::fs::rename(&staged, &published).map_err(|error| {
            format!(
                "publish verified dSYM {} -> {}: {error}",
                staged.display(),
                published.display()
            )
        })?;
        output.dsym = Some(published);
    }
    if let Some(staged) = output.dsym_zip.take() {
        let name = staged
            .file_name()
            .ok_or("staged dSYM zip has no file name")?;
        let published = out_dir.join(name);
        match std::fs::remove_file(&published) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("replace {}: {error}", published.display())),
        }
        std::fs::rename(&staged, &published).map_err(|error| {
            format!(
                "publish verified dSYM zip {} -> {}: {error}",
                staged.display(),
                published.display()
            )
        })?;
        output.dsym_zip = Some(published);
    }
    Ok(output)
}

fn run_with_take(
    plan: &BuildPlan,
    take: &SealedCargoTake,
    symbol_out: &Path,
) -> Result<BuildOutput, String> {
    // dSYM output stays under the private release target until source and
    // dependencies have passed their post-build verification. Only then is it
    // moved into dist/, so an intentional output cannot perturb that check.
    create_private_release_directory(symbol_out, take.release_uid, "release symbol stage")?;

    // --- per-arch builds --------------------------------------------------
    // arm64 first (the native slice), then x86_64 unless --arm64-only.
    //
    // The native slice is the ordinary Trust lane, now rooted in the immutable
    // take acquired above. No --target on purpose (host proc-macros and build
    // scripts ride the same config rustflags). Provenance remains hard-gated.
    let mut slices: Vec<Vec<PathBuf>> = vec![Vec::new(); PACKAGES.len()];
    let t = Instant::now();
    println!(
        "==> [{ARM64}] Trust toolchain (+t): native build (SOURCE_DATE_EPOCH={})",
        plan.build_number
    );
    let mut built: Vec<&str> = Vec::new();
    for (i, (pkg, bin, _)) in PACKAGES.iter().enumerate() {
        // One crate can ship several bins (aterm-agent → fleet + drive);
        // `cargo build -p` produces them all, so build each package once.
        if !built.contains(pkg) {
            build_one(plan, take, pkg, None)?;
            built.push(pkg);
        }
        // Native build (no --target) → artifacts under target/release.
        slices[i].push(take.target_dir.join("release").join(bin));
    }
    println!("    arm64 done in {}", fmt_elapsed(t));

    if !plan.arm64_only {
        // x86_64 compat slice: upstream stable via rustup's target std — THE
        // one exception to the single Trust lane; see the module docs for why
        // (it is NOT that Trust lacks an x86_64 std; it has one). NOT auto-added here: spec
        // decision 18 — print the remediation and require an explicit
        // --arm64-only to ship single-arch.
        let t = Instant::now();
        println!("==> [{X86_64}] upstream stable (+r): --target compat slice");
        require_rustup_target(X86_64)?;
        let mut built: Vec<&str> = Vec::new();
        for (i, (pkg, bin, _)) in PACKAGES.iter().enumerate() {
            if !built.contains(pkg) {
                build_one(plan, take, pkg, Some(X86_64))?;
                built.push(pkg);
            }
            slices[i].push(target_bin(&take.target_dir, X86_64, bin));
        }
        println!("    x86_64 done in {}", fmt_elapsed(t));
    }

    if let Some(expected) = &plan.expected_update_pin_sha256 {
        verify_built_slice_update_pins(&plan.repo_root, &slices[0], expected)?;
        println!(
            "    updater pin: every architecture slice embeds {}…",
            &expected[..12]
        );
    }

    // --- lipo to universal (single-arch pass-through) ----------------------
    let universal = take.target_dir.join("universal");
    create_private_release_directory(&universal, take.release_uid, "release universal stage")?;
    let mut fat: Vec<PathBuf> = Vec::new();
    for (i, (_, _, ship_name)) in PACKAGES.iter().enumerate() {
        let out = universal.join(ship_name);
        lipo_or_copy(&slices[i], &out)?;
        fat.push(out);
    }
    let archs = lipo_archs(&fat[0])?;
    if plan.arm64_only {
        println!("    single-arch binary ({archs}) — NOT universal (--arm64-only)");
    } else {
        println!("    universal binary: {archs}");
    }

    // --- dSYM from the UN-stripped binary ----------------------------------
    let (dsym, dsym_zip) = extract_dsym(&fat[0], symbol_out, &plan.short_version, &plan.repo_root)?;

    // --- strip the SHIPPED copies ------------------------------------------
    // Symbols live in the archived .dSYM (matched by the Mach-O UUID, which
    // strip preserves); the bundle binaries stay small while crash reports
    // remain symbolicatable. Stripping a private COPY keeps the unstripped
    // original available to dsymutil until this source take is released.
    let ship_dir = universal.join("ship");
    create_private_release_directory(&ship_dir, take.release_uid, "release ship stage")?;
    let mut shipped: Vec<PathBuf> = Vec::new();
    for (src, (_, _, ship_name)) in fat.iter().zip(PACKAGES.iter()) {
        let dst = ship_dir.join(ship_name);
        std::fs::copy(src, &dst).map_err(|e| format!("copy {}: {e}", src.display()))?;
        make_executable(&dst)?;
        // `strip -x`: local symbols only — matches build-app.sh; a failure here
        // is tolerated (`|| true` in the script) because an unstripped binary
        // is merely bigger, never wrong.
        let _ = Command::new("strip")
            .arg("-x")
            .arg(&dst)
            .current_dir(&plan.repo_root)
            .output();
        shipped.push(dst);
    }

    // Bind the ACTUAL post-strip bytes, not merely cargo's pre-lipo inputs.
    // A tolerated strip failure still reaches this mandatory proof; a successful
    // strip/lipo that drops or changes either architecture's dedicated section
    // fails closed. Fat slices are extracted as data and never executed.
    if let Some(expected) = &plan.expected_update_pin_sha256 {
        verify_shipped_update_pin_slices(&shipped[0], &archs, plan.arm64_only, expected)?;
        println!("    updater pin: every final shipped architecture structurally verified");
    }

    // --- compiler provenance HARD GATE ---------------------------------------
    // Ask the BUILT binary which compiler produced it: build.rs bakes the
    // `$RUSTC -vV` probe into `build_info::compiler_summary()`, and
    // `--diagnose` prints it as the `compiler:` line (`rustc <release>
    // (<slug>) · trust|rust · <profile> · trust_verify on|off`) — ground
    // truth for the shipped bytes. Single-lane invariant: the native slice
    // MUST be a Trust build (`· trust ·`). Anything else means the toolchain
    // file was bypassed (broken rustup link, stale env) — that is a broken
    // toolchain, not a fallback, so the cut refuses to continue.
    let diagnose = match Command::new(&shipped[0])
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
    let version_output = Command::new(&shipped[0])
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
    if !compiler_line.contains("\u{00b7} trust \u{00b7}") {
        return Err(format!(
            "compiler provenance gate: the native slice reports {compiler_line:?} — not a \
             Trust-flavor build. The repo compiles with Trust always; the native lane must \
             have been driven by something other than the stage2 targo/trustc (stale env, \
             wrong TRUST_STAGE2_BIN?). Fix the toolchain resolution and recut"
        ));
    }
    println!("    compiler: {compiler_line}  (Trust provenance gate passed)");

    Ok(BuildOutput {
        aterm: shipped[0].clone(),
        archs,
        compiler_line,
        dsym,
        dsym_zip,
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
/// `target` = Some(triple) for the upstream-stable compat slice.
///
/// Output is streamed (not captured): release builds run for minutes and the
/// operator needs cargo's own progress; on failure cargo has already printed
/// the errors, so the returned Err only names the step.
fn build_one(
    plan: &BuildPlan,
    take: &SealedCargoTake,
    pkg: &str,
    target: Option<&str>,
) -> Result<(), String> {
    for config in ["/.cargo/config.toml", "/.cargo/config"] {
        match std::fs::symlink_metadata(config) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect root Cargo config {config}: {error}")),
            Ok(_) => {
                return Err(format!(
                    "refusing ambient root Cargo config during sealed release build: {config}"
                ));
            }
        }
    }
    // The build driver, per lane. Native slice: `targo` from the Trust stage2
    // tool dir — never a PATH `cargo`, which since the stock-name purge is a
    // rustup shim the repo's toolchain pin can no longer satisfy. Compat
    // slice: upstream stable's `cargo` via the rustup shim — the ONE
    // deliberately stock lane (see the module docs; Trust DOES have an
    // x86_64-apple-darwin std, so that is not the reason).
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
    let mut cmd = Command::new(&driver_path);
    cmd.current_dir("/");
    // Targo's explicit unverified lane, the exact sealed dependency source,
    // and offline resolution are one tested argument vector for both arches.
    cmd.args(cargo_build_args(
        &take.workspace_config,
        &take.cargo_config,
        &take.source_root.join("Cargo.toml"),
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
        .env("CARGO_HOME", &take.cargo_home);

    // THE build-number conduit (spec §2 propagation): build.rs reads
    // SOURCE_DATE_EPOCH for ATERM_BUILD_NUMBER **and** ATERM_BUILD_TIME, and a
    // valid epoch WINS over its live-git fallback — so the binary, the plist
    // stamp, and the manifest all carry the one claimed u64.
    cmd.env("SOURCE_DATE_EPOCH", plan.build_number.to_string());
    cmd.env("CARGO_TARGET_DIR", &take.target_dir);

    // Real .dSYM: line-table debug info AND no stripping (the release
    // profile's strip=true would erase the debug map dsymutil follows).
    // Scoped to THIS build via cargo profile env overrides — the global
    // profile is unchanged. Unlike build-app.sh's `:-` defaults these are set
    // UNCONDITIONALLY: an ambient CARGO_PROFILE_RELEASE_STRIP=true would
    // silently kill the dSYM, and a release cutter must not be steerable by
    // stale shell state.
    cmd.env("CARGO_PROFILE_RELEASE_DEBUG", "1");
    cmd.env("CARGO_PROFILE_RELEASE_STRIP", "false");

    // NO trust anchors are injected into the child build. They used to arrive here
    // from ~/.aterm/release.conf so `option_env!` could bake them in, which made what
    // a binary trusts a property of the shell that compiled it. They are committed
    // constants now (`aterm_update_core::pins`) and the child compiles them in
    // directly; exporting them would recreate a second, disagreeing source.

    // Lane env. Native slice: NOTHING beyond the driver itself — the resolved
    // targo supplies trustc and .cargo/config.toml (which targo reads, same
    // discovery as cargo) the verification opt-out; adding env here would
    // create a second, undocumented lane. Compat slice: RUSTUP_TOOLCHAIN=stable
    // overrides the toolchain file (a deliberate stock lane, not a Trust
    // limitation — see the module docs), and inherited
    // RUSTC/RUSTC_BOOTSTRAP/RUSTFLAGS are scrubbed on BOTH lanes — a release
    // cutter must not be steerable by stale shell state (same rule as the
    // CARGO_PROFILE pins above).
    cmd.env_remove("RUSTC")
        .env_remove("RUSTC_BOOTSTRAP")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("ATERM_APP_RELEASE_VERSION");
    // The value comes from the validated release context, never the ambient
    // environment or release.conf. Both architecture builds receive the same
    // exact app identity while Cargo.toml remains on its source-version line.
    cmd.env("ATERM_APP_RELEASE_VERSION", &plan.short_version);
    let lane = if target.is_some() {
        cmd.env("RUSTUP_HOME", release_rustup_home(&home)?)
            .env("RUSTUP_TOOLCHAIN", "stable");
        "upstream stable (+r) compat"
    } else {
        "Trust (+t)"
    };

    println!(
        "==> {driver_name} build --release {}-p {pkg}  [{lane}]",
        target.map(|t| format!("--target {t} ")).unwrap_or_default()
    );
    let status = cmd
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("spawn {driver_name} for {pkg}: {e}"))?;
    if !status.success() {
        // Hard error (release cutter): a warn-and-skip would let a release
        // ship with a missing slice or missing atpkg/aterm-ctl/aterm-cli.
        return Err(format!("{driver_name} build -p {pkg} failed ({status})"));
    }
    Ok(())
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
        .map_err(|e| format!("rustup not runnable ({e}) — install rustup or pass --arm64-only"))?;
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
             `rustup +stable target add {triple}` \
             (or pass --arm64-only to ship a single-arch build)"
        ));
    }
    Ok(())
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
    value.len() == UPDATE_PIN_RECORD_LEN as usize
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
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

fn expected_lipo_architectures(arm64_only: bool) -> &'static [&'static str] {
    if arm64_only {
        &[LIPO_ARM64]
    } else {
        &[LIPO_ARM64, LIPO_X86_64]
    }
}

fn validate_lipo_architectures(archs: &str, arm64_only: bool) -> Result<Vec<&str>, String> {
    let observed: Vec<&str> = archs.split_whitespace().collect();
    let expected = expected_lipo_architectures(arm64_only);
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
    arm64_only: bool,
    expected_pin: &str,
) -> Result<(), String> {
    let architectures = validate_lipo_architectures(archs, arm64_only)?;
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
    validate_final_slice_records(
        expected_pin,
        expected_lipo_architectures(arm64_only),
        &records,
    )
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

/// The only lines `aterm --version` may print after its identity line — the S12
/// "which copy runs" report, spelled by `aterm_update::which_copy::WhichCopy::lines`.
const WHICH_COPY_LINE_PREFIXES: &[&str] = &["running: ", "another copy: "];

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

/// Where a `--target` build put a cargo binary (by its [`PACKAGES`] bin name;
/// the SHIP rename happens at the lipo/copy step).
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
    use std::sync::{Arc, Barrier};

    use super::{
        BuildOutput, LC_SEGMENT_64, MACH_HEADER_64_LEN, MACH_MAGIC_64, MH_EXECUTE, PrivateSliceDir,
        RELEASE_SYSTEM_PATH, SECTION_64_LEN, SEGMENT_COMMAND_64_LEN, cargo_build_args,
        cleanup_release_target_residue_with, create_private_release_directory, current_release_uid,
        fresh_lease_token, is_lower_hex, parse_thin_macho_update_pin,
        prepare_release_target_parent, publish_verified_binary, publish_verified_symbols,
        release_tool_path, resolve_release_rustup_shim_dir, validate_app_version_reports,
        validate_cli_app_version, validate_embedded_update_pin, validate_final_slice_records,
        validate_lipo_architectures, validate_named_cli_app_version,
        validate_slice_update_pin_reports, write_release_target_owner,
    };

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
        let workspace_config = std::path::Path::new("/private/take/.cargo/config.toml");
        let source_config =
            std::path::Path::new("/private/take/.aterm-proof-registry/.cargo/config.toml");
        let manifest = std::path::Path::new("/private/take/Cargo.toml");
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

    #[test]
    fn release_take_tokens_are_unique_canonical_hex() {
        let first = fresh_lease_token().unwrap();
        let second = fresh_lease_token().unwrap();
        assert!(is_lower_hex(&first, 64));
        assert!(is_lower_hex(&second, 64));
        assert_ne!(first, second);
    }

    #[test]
    fn release_target_gc_removes_only_positively_dead_owners() {
        let scratch = PrivateSliceDir::create().unwrap();
        let parent = scratch.0.join("release-takes");
        std::fs::create_dir(&parent).unwrap();
        let take = "a".repeat(64);
        let token = "b".repeat(64);
        for pid in 1..=6 {
            std::fs::create_dir(parent.join(format!("{take}-{pid}-{token}"))).unwrap();
        }
        let abandoned = parent.join(format!("{take}-7-{token}"));
        std::fs::create_dir(&abandoned).unwrap();
        std::fs::create_dir(parent.join("unrelated")).unwrap();

        cleanup_release_target_residue_with(&parent, |pid| {
            Ok((pid <= 6).then(|| "live process".to_owned()))
        })
        .unwrap();

        let retained = std::fs::read_dir(&parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != "unrelated")
            .count();
        assert_eq!(retained, 6);
        assert!(!abandoned.exists());
        assert!(parent.join("unrelated").is_dir());
        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for pid in 1..=6 {
            assert_eq!(
                std::fs::metadata(parent.join(format!("{take}-{pid}-{token}")))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn release_build_directories_and_owner_are_private_and_owned() {
        let scratch = PrivateSliceDir::create().unwrap();
        let uid = current_release_uid().unwrap();
        let parent = scratch.0.join("release-takes");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let target = parent.join(format!("{}-1-{}", "a".repeat(64), "b".repeat(64)));
        create_private_release_directory(&target, uid, "test release target").unwrap();
        let cargo_home = target.join("cargo-home");
        let temporary = target.join("tmp");
        create_private_release_directory(&cargo_home, uid, "test Cargo home").unwrap();
        create_private_release_directory(&temporary, uid, "test temporary directory").unwrap();
        write_release_target_owner(&target, 1, "test process", &"b".repeat(64)).unwrap();

        // The production parent preparation tightens an existing owned base.
        super::open_release_directory(&parent, uid, true, "test release parent").unwrap();
        for directory in [&parent, &target, &cargo_home, &temporary] {
            let metadata = std::fs::symlink_metadata(directory).unwrap();
            assert!(metadata.file_type().is_dir());
            assert_eq!(metadata.uid(), uid);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
        let owner = std::fs::symlink_metadata(target.join(".owner")).unwrap();
        assert!(owner.file_type().is_file());
        assert_eq!(owner.uid(), uid);
        assert_eq!(owner.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn release_target_gc_refuses_a_symlinked_base() {
        let scratch = PrivateSliceDir::create().unwrap();
        let outside = scratch.0.join("outside");
        let base = scratch.0.join("release-takes");
        std::fs::create_dir(&outside).unwrap();
        let sentinel = outside.join("keep");
        std::fs::write(&sentinel, "not residue").unwrap();
        symlink(&outside, &base).unwrap();

        let error = cleanup_release_target_residue_with(&base, |_| Ok(None)).unwrap_err();
        assert!(error.contains("not a real directory"), "{error}");
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "not residue");
    }

    #[test]
    fn release_target_parent_refuses_a_symlinked_target_root() {
        let scratch = PrivateSliceDir::create().unwrap();
        let repo = scratch.0.join("repo");
        let outside = scratch.0.join("outside-target");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, repo.join("target")).unwrap();

        let error = prepare_release_target_parent(&repo).unwrap_err();
        assert!(error.contains("not a real directory"), "{error}");
        assert!(!outside.join("release-takes").exists());
    }

    #[test]
    fn verified_binary_publication_is_atomic_and_concurrency_isolated() {
        let scratch = PrivateSliceDir::create().unwrap();
        let repo = scratch.0.join("repo");
        std::fs::create_dir(&repo).unwrap();
        prepare_release_target_parent(&repo).unwrap();
        let target = repo.join("target");
        let first_stage = target.join("first-stage");
        let second_stage = target.join("second-stage");
        std::fs::create_dir(&first_stage).unwrap();
        std::fs::create_dir(&second_stage).unwrap();
        let first_binary = first_stage.join("aterm");
        let second_binary = second_stage.join("aterm");
        std::fs::write(&first_binary, "first verified binary").unwrap();
        std::fs::write(&second_binary, "second verified binary").unwrap();

        let uid = current_release_uid().unwrap();
        let pid = std::process::id();
        let take = "a".repeat(64);
        let first_token = "b".repeat(64);
        let second_token = "c".repeat(64);
        let barrier = Arc::new(Barrier::new(3));
        let spawn_publish = |staged: std::path::PathBuf, token: String| {
            let repo = repo.clone();
            let take = take.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                publish_verified_binary(&repo, &staged, &take, pid, &token, uid)
            })
        };
        let first = spawn_publish(first_binary.clone(), first_token);
        let second = spawn_publish(second_binary.clone(), second_token);
        barrier.wait();
        let first_published = first.join().unwrap().unwrap();
        let second_published = second.join().unwrap().unwrap();

        assert_ne!(first_published, second_published);
        assert_eq!(
            std::fs::read_to_string(&first_published).unwrap(),
            "first verified binary"
        );
        assert_eq!(
            std::fs::read_to_string(&second_published).unwrap(),
            "second verified binary"
        );
        for (staged, published) in [
            (&first_binary, &first_published),
            (&second_binary, &second_published),
        ] {
            let staged = std::fs::symlink_metadata(staged).unwrap();
            let published_metadata = std::fs::symlink_metadata(published).unwrap();
            assert!(published_metadata.file_type().is_file());
            assert_eq!(published_metadata.uid(), uid);
            assert_eq!(published_metadata.permissions().mode() & 0o777, 0o500);
            assert_eq!(
                (staged.dev(), staged.ino()),
                (published_metadata.dev(), published_metadata.ino())
            );
            let directory = std::fs::symlink_metadata(published.parent().unwrap()).unwrap();
            assert_eq!(directory.uid(), uid);
            assert_eq!(directory.permissions().mode() & 0o777, 0o700);
        }
        let product_parent = std::fs::symlink_metadata(target.join("release-products")).unwrap();
        assert_eq!(product_parent.uid(), uid);
        assert_eq!(product_parent.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn verified_binary_publication_refuses_a_symlinked_product_base() {
        let scratch = PrivateSliceDir::create().unwrap();
        let repo = scratch.0.join("repo");
        let outside = scratch.0.join("outside-products");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(&outside).unwrap();
        prepare_release_target_parent(&repo).unwrap();
        let sentinel = outside.join("keep");
        std::fs::write(&sentinel, "outside").unwrap();
        symlink(&outside, repo.join("target/release-products")).unwrap();
        let staged = repo.join("target/staged-aterm");
        std::fs::write(&staged, "verified").unwrap();

        let error = publish_verified_binary(
            &repo,
            &staged,
            &"a".repeat(64),
            std::process::id(),
            &"b".repeat(64),
            current_release_uid().unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("not a real directory"), "{error}");
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "outside");
        assert!(!outside.join("aterm").exists());
    }

    #[test]
    fn verified_binary_publication_refuses_an_unowned_destination() {
        let scratch = PrivateSliceDir::create().unwrap();
        let repo = scratch.0.join("repo");
        std::fs::create_dir(&repo).unwrap();
        prepare_release_target_parent(&repo).unwrap();
        let staged = repo.join("target/staged-aterm");
        std::fs::write(&staged, "verified").unwrap();
        let uid = current_release_uid().unwrap();
        let wrong_uid = if uid == 0 { 1 } else { 0 };

        let error = publish_verified_binary(
            &repo,
            &staged,
            &"a".repeat(64),
            std::process::id(),
            &"b".repeat(64),
            wrong_uid,
        )
        .unwrap_err();
        assert!(error.contains("owned by uid"), "{error}");
        assert!(!repo.join("target/release-products").exists());
    }

    #[test]
    fn verified_symbols_publish_only_from_the_private_stage() {
        let scratch = PrivateSliceDir::create().unwrap();
        let stage = scratch.0.join("stage");
        let out = scratch.0.join("dist");
        let dsym = stage.join("aterm.dSYM");
        let zip = stage.join("aterm-0.67.0-dSYM.zip");
        std::fs::create_dir_all(&dsym).unwrap();
        std::fs::write(dsym.join("symbol"), "verified").unwrap();
        std::fs::write(&zip, "archive").unwrap();

        let published = publish_verified_symbols(
            BuildOutput {
                aterm: scratch.0.join("aterm"),
                archs: "arm64".into(),
                compiler_line: "trust".into(),
                dsym: Some(dsym),
                dsym_zip: Some(zip),
            },
            &out,
            current_release_uid().unwrap(),
        )
        .unwrap();

        assert_eq!(published.dsym, Some(out.join("aterm.dSYM")));
        assert_eq!(published.dsym_zip, Some(out.join("aterm-0.67.0-dSYM.zip")));
        assert_eq!(
            std::fs::read_to_string(out.join("aterm.dSYM/symbol")).unwrap(),
            "verified"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("aterm-0.67.0-dSYM.zip")).unwrap(),
            "archive"
        );
        assert!(out.join(".metadata_never_index").is_file());
        let out_metadata = std::fs::symlink_metadata(out).unwrap();
        assert_eq!(out_metadata.uid(), current_release_uid().unwrap());
        assert_eq!(out_metadata.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn verified_symbols_refuse_a_symlinked_output_directory() {
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

        let error = match publish_verified_symbols(
            BuildOutput {
                aterm: scratch.0.join("aterm"),
                archs: "arm64".into(),
                compiler_line: "trust".into(),
                dsym: Some(dsym.clone()),
                dsym_zip: None,
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
                  /Users//ana/Applications/aterm.app (0.1.0) \xe2\x80\x94 not the one running; \
                  the updater updates only this one\n"
            )
            .is_ok()
        );
        assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.0\nrunning: /x/aterm\n").is_ok());
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

        assert!(validate_lipo_architectures("x86_64 arm64", false).is_ok());
        assert!(validate_lipo_architectures("arm64", true).is_ok());
        for invalid in ["arm64", "x86_64", "arm64 arm64", "arm64 x86_64 ppc64"] {
            assert!(validate_lipo_architectures(invalid, false).is_err());
        }
    }
}
