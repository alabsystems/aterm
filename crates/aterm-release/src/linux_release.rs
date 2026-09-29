// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Linux worker and immutable handoff to the ONE release cutter.
//! A handoff is local provenance, not a release signature. Only `ship cut`
//! admits its exact bytes into the appcast before signing and draft upload.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_update_core::linux::{LINUX_BINARY_MAX_BYTES, LinuxTarget};
use aterm_update_core::{Manifest, sha256_file};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub schema: u32,
    pub version: String,
    pub build_number: u64,
    pub commit: String,
    pub target: String,
    pub asset: String,
    pub sha256: String,
    pub size: u64,
    pub source_fingerprint: String,
    pub compiler_sha256: String,
    pub driver_sha256: String,
    pub compiler_library_sha256: String,
    pub compiler_version: String,
    pub compiler_provenance_sha256: String,
}

/// The directory is fixed before claim. The first successful import freezes
/// the complete pair, not just paths which could change under a resumed cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub directory: PathBuf,
    pub targets: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// The native hosts the cutter drives itself (`--linux-worker`), journaled
    /// with the rest of the handoff so a resumed cut still knows where each
    /// architecture is built. Empty: every worker is run by hand.
    #[serde(default)]
    pub workers: Vec<Worker>,
}

/// A native Linux host the cutter drives over ssh: `ARCH=DESTINATION:REPOSITORY`
/// on the command line (`aarch64=buildhost:/home/me/src/aterm`).
///
/// The cutter runs `ship linux-build` there in a throwaway worktree of
/// REPOSITORY detached at the claim commit, copies the executable and its
/// provenance receipt back into the handoff directory, and removes the remote
/// run. The pair then goes through the SAME [`Handoff::import`] a hand-carried
/// pair does — identity, size, digest and Trust provenance, bound to the claim —
/// so a worker is trusted exactly as much as an operator carrying its pair by
/// hand would be: no more (a mistaken or stale pair is refused) and no less (a
/// compromised worker HOST can still build hostile bytes with a matching
/// receipt, and only the host's own integrity rules that out). No credential
/// crosses to it: agent and X11 forwarding are switched off on every call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    /// The Linux target triple this host builds natively.
    pub target: String,
    /// The ssh destination (`host` or `user@host`, an ssh config alias included).
    pub destination: String,
    /// The aterm checkout on that host that the throwaway worktree comes from:
    /// absolute, or `~/`-relative to the remote login.
    pub repository: String,
}

/// The ssh options on every worker call. `BatchMode`: never a prompt the
/// launchd-hosted cut cannot answer. The timeouts: a dead host or a dropped
/// connection fails the step instead of holding the release lock forever. The
/// forwarding switches: a user's `ForwardAgent yes` must not hand the
/// publisher's agent to a build host.
const SSH_OPTIONS: [&str; 14] = [
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=20",
    "-o",
    "ServerAliveInterval=30",
    "-o",
    "ServerAliveCountMax=6",
    "-o",
    "ForwardAgent=no",
    "-o",
    "ForwardX11=no",
    "-o",
    "ClearAllForwardings=yes",
];

/// The shell a worker runs, fed to `bash -l -s` on stdin so no script text is
/// ever re-quoted through ssh's argv join. Its arguments are validated
/// [`Worker`] fields and cut identity, each a single shell-safe word. The build
/// talks on stderr (streamed to the operator); stdout's LAST line is the
/// private directory holding the pair (a login file may print before it).
///
/// The body is one function called with stdin from `/dev/null`: bash parses
/// all of it before running any of it, so nothing the build runs can read the
/// rest of the script off the pipe. Each run gets its own `mktemp -d`, so an
/// orphaned earlier run of the same build cannot delete or overwrite this one.
/// `umask 077` because the worker refuses a group- or other-writable target
/// root, and a login's `umask 002` (Ubuntu's default) made exactly that —
/// measured on the first live run.
const WORKER_SCRIPT: &str = r#"main() {
set -eu
umask 077
repo=$1 commit=$2 version=$3 build=$4 arch=$5
case "${XDG_CACHE_HOME:-}" in
/*) cache=$XDG_CACHE_HOME ;;
*) cache=$HOME/.cache ;;
esac
root="$cache/aterm-linux-worker"
mkdir -p "$root"
chmod 700 "$root"
run=$(mktemp -d "$root/run-$build-$arch.XXXXXX")
work="$run/src"
out="$run/out"
cd "$repo"
git fetch --quiet origin
git worktree add --quiet --detach "$work" "$commit"
trap 'cd "$repo" && git worktree remove --force "$work" >/dev/null 2>&1 || true' EXIT
git -C "$work" submodule update --init --quiet vendor/astream
mkdir -m 700 "$out"
cd "$work"
targo --unverified ship linux-build --version "$version" --build-number "$build" \
    --commit "$commit" --out "$out" >&2
printf '%s\n' "$out"
}
main "$@" </dev/null
"#;

/// One shell-safe word: nothing a remote shell would split, glob, expand or
/// read as an option.
fn shell_word(value: &str, extra: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c) || extra.contains(c))
}

/// Whether `directory` already holds this cut's valid pair for `target` —
/// the receipt parses, names exactly this version, build, commit and
/// architecture, and the executable matches it. A pair from another build (a
/// dry run's, a failed claim's) or a torn copy is NOT current.
fn pair_is_current(
    directory: &Path,
    target: LinuxTarget,
    version: &str,
    build: u64,
    commit: &str,
) -> bool {
    let name = target.asset_name(version);
    let receipt = directory.join(format!("{name}.handoff.toml"));
    let Ok(metadata) = fs::symlink_metadata(&receipt) else {
        return false;
    };
    if !metadata.file_type().is_file() || metadata.len() > 65536 {
        return false;
    }
    let Ok(text) = fs::read_to_string(&receipt) else {
        return false;
    };
    let Ok(artifact) = aterm_toml::from_str::<Artifact>(&text) else {
        return false;
    };
    artifact.validate(version, build, commit).is_ok()
        && artifact.target == target.triple()
        && artifact.verify_file(&directory.join(&name)).is_ok()
}

impl Worker {
    /// Parse `ARCH=DESTINATION:REPOSITORY`, where ARCH is `aarch64` or `x86_64`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let usage = || {
            format!(
                "--linux-worker {spec:?}: expected ARCH=DESTINATION:REPOSITORY, e.g. \
                 aarch64=buildhost:/home/me/src/aterm"
            )
        };
        let (arch, rest) = spec.split_once('=').ok_or_else(usage)?;
        let (destination, repository) = rest.split_once(':').ok_or_else(usage)?;
        let target = match arch {
            "aarch64" => LinuxTarget::Aarch64,
            "x86_64" => LinuxTarget::X86_64,
            other => return Err(format!("unsupported Linux worker architecture {other:?}")),
        };
        if !shell_word(destination, "@") {
            return Err(format!(
                "--linux-worker destination {destination:?} must be a plain ssh host or user@host"
            ));
        }
        let path_ok = (repository.starts_with('/') || repository.starts_with("~/"))
            && shell_word(repository.trim_start_matches('~'), "/")
            && !repository.split('/').any(|part| part == "..");
        if !path_ok {
            return Err(format!(
                "--linux-worker repository {repository:?} must be an absolute or ~/ path of \
                 plain characters"
            ));
        }
        Ok(Self {
            target: target.triple().to_string(),
            destination: destination.to_string(),
            repository: repository.to_string(),
        })
    }

    /// Build this worker's architecture at the claim and copy its pair into
    /// `directory` — unless this cut's valid pair is already there (a resumed
    /// cut, or an operator who carried it by hand). Anything else under those
    /// names — another build's pair, a torn copy — is removed and rebuilt, so a
    /// stale file can never stop a cut after its claim.
    fn run(&self, directory: &Path, version: &str, build: u64, commit: &str) -> Result<(), String> {
        let target = target(&self.target)?;
        check_identity(version, build, commit)?;
        let name = target.asset_name(version);
        let receipt = format!("{name}.handoff.toml");
        if pair_is_current(directory, target, version, build, commit) {
            println!(
                "==> Linux worker {}: this cut's pair is already in {}; not rebuilding",
                target.triple(),
                directory.display()
            );
            return Ok(());
        }
        for stale in [&name, &receipt] {
            match fs::remove_file(directory.join(stale)) {
                Ok(()) => println!(
                    "==> Linux worker {}: removed a stale {stale}",
                    target.triple()
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("remove stale Linux handoff file {stale}: {e}")),
            }
        }
        let arch = self.target.split('-').next().unwrap_or(&self.target);
        println!(
            "==> Linux worker {}: building v{version} build {build} at {} on {} ({}) — \
             its output follows",
            target.triple(),
            &commit[..12],
            self.destination,
            self.repository
        );
        let mut child = Command::new("ssh")
            .args(SSH_OPTIONS)
            .args(["-a", "-x", "--", &self.destination])
            .args(["bash", "-l", "-s", "--", &self.repository, commit, version])
            .arg(build.to_string())
            .arg(arch)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                format!(
                    "could not start ssh to Linux worker {}: {e}",
                    self.destination
                )
            })?;
        child
            .stdin
            .take()
            .ok_or("Linux worker ssh has no stdin")?
            .write_all(WORKER_SCRIPT.as_bytes())
            .map_err(|e| format!("send the Linux worker script: {e}"))?;
        let output = child
            .wait_with_output()
            .map_err(|e| format!("wait for Linux worker {}: {e}", self.destination))?;
        if !output.status.success() {
            return Err(format!(
                "Linux worker {} on {} failed ({}); its output is above. Fix it and resume the \
                 cut, or build there by hand into {}",
                target.triple(),
                self.destination,
                output.status,
                directory.display()
            ));
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|e| format!("Linux worker output is not UTF-8: {e}"))?;
        let remote_out = text.lines().last().unwrap_or_default();
        let remote_run = remote_out.strip_suffix("/out").unwrap_or_default();
        if !remote_out.starts_with('/')
            || !shell_word(remote_out, "/")
            || !remote_run.contains("/aterm-linux-worker/run-")
        {
            return Err(format!(
                "Linux worker on {} did not name its output directory (got {remote_out:?})",
                self.destination
            ));
        }
        // Into a private staging directory first, then renamed into place
        // receipt LAST: an interrupted copy never leaves a pair that looks whole.
        let staging = directory.join(format!(".worker-{arch}"));
        match fs::remove_dir_all(&staging) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("clear {}: {e}", staging.display())),
        }
        fs::create_dir(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;
        let status = Command::new("scp")
            .args(SSH_OPTIONS)
            .args(["-q", "--"])
            .arg(format!("{}:{remote_out}/{name}", self.destination))
            .arg(format!("{}:{remote_out}/{receipt}", self.destination))
            .arg(&staging)
            .status()
            .map_err(|e| format!("could not start scp from Linux worker: {e}"))?;
        if !status.success() {
            return Err(format!(
                "copying the Linux worker's pair from {}:{remote_out} failed ({status})",
                self.destination
            ));
        }
        for file in [&name, &receipt] {
            fs::rename(staging.join(file), directory.join(file))
                .map_err(|e| format!("move the Linux worker's {file} into place: {e}"))?;
        }
        let _ = fs::remove_dir(&staging);
        // Best effort: the remote run directory is scratch once the pair is here.
        let _ = Command::new("ssh")
            .args(SSH_OPTIONS)
            .args([
                "-a",
                "-x",
                "--",
                &self.destination,
                "rm",
                "-rf",
                "--",
                remote_run,
            ])
            .stdin(std::process::Stdio::null())
            .status();
        println!(
            "==> Linux worker {}: pair copied into {}",
            target.triple(),
            directory.display()
        );
        Ok(())
    }
}

pub const TARGETS: [LinuxTarget; 2] = [LinuxTarget::X86_64, LinuxTarget::Aarch64];

pub fn manifest_asset_names(manifest: &Manifest) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for target in TARGETS {
        if let Some(artifact) = manifest.linux_artifact(target)? {
            names.push(artifact.name.to_string());
        }
    }
    Ok(names)
}

pub fn verify_manifest_handoff(
    manifest: &Manifest,
    handoff: Option<&Handoff>,
) -> Result<(), String> {
    let mut expected = manifest.clone();
    expected.linux_x86_64 = None;
    expected.linux_x86_64_sha256 = None;
    expected.linux_x86_64_size = None;
    expected.linux_aarch64 = None;
    expected.linux_aarch64_sha256 = None;
    expected.linux_aarch64_size = None;
    if let Some(handoff) = handoff {
        handoff.stamp(&mut expected)?;
    }
    for target in TARGETS {
        let actual = manifest.linux_artifact(target)?;
        let wanted = expected.linux_artifact(target)?;
        if actual.map(|a| (a.name, a.sha256, a.size)) != wanted.map(|a| (a.name, a.sha256, a.size))
        {
            return Err("signed Linux fields disagree with the cut's frozen worker handoff".into());
        }
    }
    Ok(())
}

fn target(value: &str) -> Result<LinuxTarget, String> {
    TARGETS
        .into_iter()
        .find(|t| t.triple() == value)
        .ok_or_else(|| format!("unsupported Linux handoff target {value:?}"))
}

fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub fn check_identity(version: &str, build: u64, commit: &str) -> Result<(), String> {
    crate::ledger::check_version_shape(version).map_err(|e| e.to_string())?;
    if build == 0 || !hex(commit, 40) {
        return Err(
            "Linux worker requires a positive claimed build and full lowercase source commit"
                .into(),
        );
    }
    Ok(())
}

impl Artifact {
    pub fn validate(&self, version: &str, build: u64, commit: &str) -> Result<(), String> {
        check_identity(version, build, commit)?;
        let target = target(&self.target)?;
        if self.schema != 1
            || self.version != version
            || self.build_number != build
            || self.commit != commit
            || self.asset != target.asset_name(version)
            || !(64..=LINUX_BINARY_MAX_BYTES).contains(&self.size)
            || ![
                &self.sha256,
                &self.source_fingerprint,
                &self.compiler_sha256,
                &self.driver_sha256,
                &self.compiler_library_sha256,
                &self.compiler_provenance_sha256,
            ]
            .into_iter()
            .all(|v| hex(v, 64))
            || !self
                .compiler_version
                .lines()
                .any(|line| line.starts_with("trust: "))
            || !self
                .compiler_version
                .lines()
                .any(|line| line == format!("host: {}", target.triple()))
        {
            return Err(
                "Linux handoff has mismatched source/build/target or invalid Trust provenance"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn verify_file(&self, path: &Path) -> Result<(), String> {
        let metadata =
            fs::symlink_metadata(path).map_err(|e| format!("inspect {}: {e}", path.display()))?;
        if !metadata.file_type().is_file() || metadata.len() != self.size {
            return Err(format!(
                "Linux artifact is not a regular file of the claimed size: {}",
                path.display()
            ));
        }
        let mut header = [0u8; 64];
        File::open(path)
            .and_then(|mut file| file.read_exact(&mut header))
            .map_err(|e| e.to_string())?;
        target(&self.target)?.validate_elf_header(&header)?;
        if sha256_file(path)? != self.sha256 {
            return Err(format!("Linux artifact hash changed: {}", path.display()));
        }
        Ok(())
    }
}

impl Handoff {
    pub fn validate_identity(
        &self,
        version: &str,
        build: u64,
        commit: &str,
        complete: bool,
    ) -> Result<(), String> {
        check_identity(version, build, commit)?;
        if !self.directory.is_absolute()
            || self.directory.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || self.targets.is_empty()
            || self.targets.len() > TARGETS.len()
            || self.targets.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err("Linux handoff has an unsafe directory or noncanonical target set".into());
        }
        for requested in &self.targets {
            target(requested)?;
        }
        if (complete || !self.artifacts.is_empty()) && self.artifacts.len() != self.targets.len() {
            return Err("Linux handoff lacks the complete frozen target set".into());
        }
        for (artifact, requested) in self.artifacts.iter().zip(&self.targets) {
            artifact.validate(version, build, commit)?;
            if &artifact.target != requested {
                return Err("Linux handoff target order/identity mismatch".into());
            }
        }
        Ok(())
    }
    pub fn new(directory: &Path, targets: &[String], workers: &[String]) -> Result<Self, String> {
        let directory = directory
            .canonicalize()
            .map_err(|e| format!("Linux handoff directory: {e}"))?;
        private_directory(&directory)?;
        let mut targets = if targets.is_empty() {
            TARGETS.iter().map(|t| t.triple().to_string()).collect()
        } else {
            targets.to_vec()
        };
        targets.sort();
        if targets.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("duplicate Linux target request".into());
        }
        for requested in &targets {
            target(requested)?;
        }
        let workers = workers
            .iter()
            .map(|spec| Worker::parse(spec))
            .collect::<Result<Vec<_>, _>>()?;
        for (i, worker) in workers.iter().enumerate() {
            if !targets.contains(&worker.target) {
                return Err(format!(
                    "--linux-worker builds {}, which this cut does not declare (--linux-target)",
                    worker.target
                ));
            }
            if workers[..i].iter().any(|w| w.target == worker.target) {
                return Err(format!("two --linux-worker hosts for {}", worker.target));
            }
        }
        Ok(Self {
            directory,
            targets,
            artifacts: Vec::new(),
            workers,
        })
    }

    /// Run every declared [`Worker`] whose pair is not in the handoff directory
    /// yet. Architectures with no worker are left for [`Self::import`] to ask
    /// the operator for, exactly as before workers existed.
    pub fn run_workers(&self, version: &str, build: u64, commit: &str) -> Result<(), String> {
        private_directory(&self.directory)?;
        for worker in &self.workers {
            worker.run(&self.directory, version, build, commit)?;
        }
        Ok(())
    }

    /// Every declared native architecture is required. The default target set
    /// is both; an explicit subset honestly advertises only those platforms.
    pub fn import(
        &mut self,
        dist: &Path,
        version: &str,
        build: u64,
        commit: &str,
    ) -> Result<(), String> {
        self.validate_identity(version, build, commit, false)?;
        private_directory(&self.directory)?;
        match fs::create_dir(dist) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create Linux release stage: {e}")),
        }
        drop(super::open_release_directory(
            dist,
            super::current_release_uid()?,
            true,
            "Linux release stage",
        )?);
        let mut found = Vec::new();
        for requested in &self.targets {
            let target = target(requested)?;
            let name = target.asset_name(version);
            let receipt_path = self.directory.join(format!("{name}.handoff.toml"));
            let metadata = fs::symlink_metadata(&receipt_path).map_err(|_| format!(
                "Linux worker output missing for {}. On that native host at commit {commit}, run: \
                 targo --unverified ship linux-build --version {version} --build-number {build} \
                 --commit {commit} --out <handoff-directory>; copy its pair into {} and resume the \
                 cut (or name the host with --linux-worker on the next cut, and the cutter runs it)",
                target.triple(), self.directory.display()))?;
            if !metadata.file_type().is_file() || metadata.len() > 65536 {
                return Err("unsafe or oversized Linux handoff receipt".into());
            }
            let artifact: Artifact = aterm_toml::from_str(
                &fs::read_to_string(&receipt_path).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("parse Linux handoff: {e}"))?;
            artifact.validate(version, build, commit)?;
            if artifact.target != target.triple() {
                return Err("Linux handoff architecture does not match its filename".into());
            }
            artifact.verify_file(&self.directory.join(&name))?;
            found.push(artifact);
        }
        if !self.artifacts.is_empty() && self.artifacts != found {
            return Err(
                "Linux worker inputs changed after this cut froze their identities; resume refused"
                    .into(),
            );
        }
        for artifact in &found {
            atomic_copy(
                &self.directory.join(&artifact.asset),
                &dist.join(&artifact.asset),
            )?;
            artifact.verify_file(&dist.join(&artifact.asset))?;
        }
        self.artifacts = found;
        Ok(())
    }

    pub fn verify_staged(
        &self,
        dist: &Path,
        version: &str,
        build: u64,
        commit: &str,
    ) -> Result<(), String> {
        self.validate_identity(version, build, commit, true)?;
        if self.targets.is_empty() || self.artifacts.len() != self.targets.len() {
            return Err("Linux handoff has not frozen every declared architecture artifact".into());
        }
        for (artifact, requested) in self.artifacts.iter().zip(&self.targets) {
            let target = target(requested)?;
            artifact.validate(version, build, commit)?;
            if artifact.target != target.triple() {
                return Err("Linux handoff duplicate or reordered target".into());
            }
            artifact.verify_file(&dist.join(&artifact.asset))?;
        }
        Ok(())
    }

    pub fn stamp(&self, manifest: &mut Manifest) -> Result<(), String> {
        for artifact in &self.artifacts {
            artifact.validate(
                &manifest.version,
                manifest.build_number,
                manifest
                    .commit
                    .as_deref()
                    .ok_or("Linux manifest requires a source commit")?,
            )?;
            match target(&artifact.target)? {
                LinuxTarget::X86_64 => {
                    manifest.linux_x86_64 = Some(artifact.asset.clone());
                    manifest.linux_x86_64_sha256 = Some(artifact.sha256.clone());
                    manifest.linux_x86_64_size = Some(artifact.size);
                }
                LinuxTarget::Aarch64 => {
                    manifest.linux_aarch64 = Some(artifact.asset.clone());
                    manifest.linux_aarch64_sha256 = Some(artifact.sha256.clone());
                    manifest.linux_aarch64_size = Some(artifact.size);
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let uid = super::current_release_uid()?;
    // Input paths may be outside the checkout. Unlike a source take, they do
    // not inherit the repository's ancestry proof, so inspect every component.
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        if !metadata.file_type().is_dir()
            || ![0, uid].contains(&metadata.uid())
            || metadata.mode() & 0o022 != 0 && !sticky_root
        {
            return Err(format!(
                "unsafe Linux handoff ancestry: {}",
                ancestor.display()
            ));
        }
    }
    drop(super::open_release_directory(
        path,
        uid,
        false,
        "Linux handoff directory",
    )?);
    Ok(())
}

#[cfg(not(unix))]
fn private_directory(_path: &Path) -> Result<(), String> {
    Err("Linux handoff validation requires Unix ownership semantics".into())
}

#[cfg(unix)]
fn atomic_copy(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = destination.with_extension(format!("stage-{}", std::process::id()));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| format!("create Linux artifact stage: {e}"))?;
    let result = (|| {
        std::io::copy(
            &mut File::open(source).map_err(|e| e.to_string())?,
            &mut output,
        )
        .map_err(|e| e.to_string())?;
        output
            .set_permissions(fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        output.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temp, destination).map_err(|e| e.to_string())?;
        File::open(destination.parent().ok_or("Linux artifact has no parent")?)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(not(unix))]
fn atomic_copy(_source: &Path, _destination: &Path) -> Result<(), String> {
    Err("Linux artifact staging requires Unix ownership semantics".into())
}

fn clean_head(repo: &Path, commit: &str) -> Result<(), String> {
    let head = output_text(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo),
        "Linux worker source HEAD",
    )?;
    let status = output_text(
        Command::new("git")
            .args(["status", "--porcelain=v1", "--untracked-files=normal"])
            .current_dir(repo),
        "Linux worker source status",
    )?;
    if head.trim() != commit || !status.trim().is_empty() {
        return Err(
            "Linux worker requires a clean checkout at exactly the requested claim commit".into(),
        );
    }
    Ok(())
}

fn output_text(command: &mut Command, what: &str) -> Result<String, String> {
    String::from_utf8(super::checked_output(command, what)?.stdout)
        .map_err(|e| format!("{what} is not UTF-8: {e}"))
}

/// A local-only worker. No credentials, signing, release API, or upload code is
/// reachable from this function. Source and dependencies use the SAME sealed
/// take as the Mac cutter, and `build_one(None)` is always the Trust native lane.
pub fn run_worker(
    repo: &Path,
    out: &Path,
    version: &str,
    build: u64,
    commit: &str,
) -> Result<(), String> {
    let native = LinuxTarget::native()
        .ok_or("linux-build requires a native x86_64 or aarch64 Linux host")?;
    check_identity(version, build, commit)?;
    clean_head(repo, commit)?;
    let git = crate::ledger::GitCli::new(repo);
    crate::gates::cutter_identity_gate(&git, commit, false).map_err(|e| e.to_string())?;
    private_directory(out)?;
    let name = native.asset_name(version);
    let receipt = out.join(format!("{name}.handoff.toml"));
    if out.join(&name).exists() || receipt.exists() {
        return Err("Linux output already exists; choose a fresh handoff directory (never overwrite provenance)".into());
    }
    let compiler_dir = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
    let compiler = compiler_dir.join("trustc");
    let driver = compiler_dir.join("targo");
    let provenance = compiler_dir
        .parent()
        .ok_or("Trust seal has no root")?
        .join("PROVENANCE");
    let compiler_version = output_text(
        Command::new(&compiler).arg("-vV"),
        "Trust compiler identity",
    )?;
    if !compiler_version
        .lines()
        .any(|line| line.starts_with("trust: "))
        || !compiler_version
            .lines()
            .any(|line| line == format!("host: {}", native.triple()))
    {
        return Err("Linux worker requires a native Trust compiler, not upstream stable or a cross override".into());
    }
    let compiler_hash = sha256_file(&compiler)?;
    let driver_hash = sha256_file(&driver)?;
    let provenance_hash = sha256_file(&provenance)?;
    let provenance_text = fs::read_to_string(&provenance).map_err(|e| e.to_string())?;
    let seal_field = |key: &str| -> Result<String, String> {
        let prefix = format!("{key}=");
        let values: Vec<_> = provenance_text
            .lines()
            .filter_map(|line| line.strip_prefix(&prefix))
            .collect();
        if values.len() != 1 {
            return Err(format!("Trust seal lacks unique {key}"));
        }
        Ok(values[0].to_string())
    };
    if seal_field("provenance_class")? != "BUILD-STAMPED-CLEAN"
        || seal_field("artifact_bin_trustc_sha256")? != compiler_hash
        || seal_field("artifact_bin_targo_sha256")? != driver_hash
    {
        return Err(
            "Trust worker needs a clean build-stamped seal whose binary hashes match provenance"
                .into(),
        );
    }
    let library_name = seal_field("artifact_driver_name")?;
    if library_name.contains(['/', '\\']) || library_name == "." || library_name == ".." {
        return Err("Trust provenance names an unsafe compiler library path".into());
    }
    let compiler_library = compiler_dir
        .parent()
        .ok_or("Trust seal has no root")?
        .join("lib")
        .join(library_name);
    let library_hash = sha256_file(&compiler_library)?;
    if library_hash != seal_field("artifact_driver_sha256")? {
        return Err("Trust compiler library differs from its build-stamped seal".into());
    }
    let mut take = super::SealedCargoTake::acquire(repo)?;
    let result = (|| {
        let plan = super::BuildPlan {
            repo_root: repo.into(),
            out_dir: out.into(),
            build_number: build,
            short_version: version.into(),
            arm64_only: true,
            expected_update_pin_sha256: None,
        };
        super::build_one(&plan, &take, "aterm", None)?;
        take.verify()?;
        clean_head(repo, commit)?;
        if sha256_file(&compiler)? != compiler_hash
            || sha256_file(&driver)? != driver_hash
            || sha256_file(&provenance)? != provenance_hash
            || sha256_file(&compiler_library)? != library_hash
        {
            return Err("Trust seal changed during the native Linux build".into());
        }
        let binary = take.target_dir.join("release/aterm");
        let version_output = Command::new(&binary)
            .arg("--version")
            .output()
            .map_err(|e| e.to_string())?;
        if !version_output.status.success() {
            return Err("native Linux binary cannot start".into());
        }
        super::validate_cli_app_version(version, &version_output.stdout)?;
        let identity = output_text(
            Command::new(&binary).args(["update", "identity"]),
            "native Linux compiled identity",
        )?;
        let identity: aterm_update_core::linux::BinaryIdentity =
            aterm_json::from_str(&identity).map_err(|e| format!("Linux identity JSON: {e}"))?;
        if identity.schema != 1
            || identity.version != version
            || identity.build_number != build
            || identity.commit != commit
            || identity.target != native.triple()
            || identity.dirty
        {
            return Err(
                "Linux executable does not report the exact clean source/build/target identity"
                    .into(),
            );
        }
        let artifact = Artifact {
            schema: 1,
            version: version.into(),
            build_number: build,
            commit: commit.into(),
            target: native.triple().into(),
            asset: name.clone(),
            sha256: sha256_file(&binary)?,
            size: fs::metadata(&binary).map_err(|e| e.to_string())?.len(),
            source_fingerprint: take.source_fingerprint.clone(),
            compiler_sha256: compiler_hash,
            driver_sha256: driver_hash,
            compiler_library_sha256: library_hash,
            compiler_version,
            compiler_provenance_sha256: provenance_hash,
        };
        artifact.validate(version, build, commit)?;
        artifact.verify_file(&binary)?;
        atomic_copy(&binary, &out.join(&name))?;
        let text = aterm_toml::to_string(&artifact).map_err(|e| e.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&receipt)
            .map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        println!(
            "Linux local handoff ready: {} (unsigned; only ship cut may publish it)",
            out.join(&name).display()
        );
        Ok(())
    })();
    let released = take.release();
    match (result, released) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), Ok(())) | (Ok(()), Err(e)) => Err(e),
        (Err(a), Err(b)) => Err(format!("{a}; additionally {b}")),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "aterm-linux-handoff-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(dir: &Path, target: LinuxTarget) -> Artifact {
        // Synthetic ELF HEADER ONLY: unit-test data, never executable output.
        let mut bytes = vec![0u8; 80];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
        let machine: u16 = if target == LinuxTarget::Aarch64 {
            183
        } else {
            62
        };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        let asset = target.asset_name("0.100.0");
        fs::write(dir.join(&asset), &bytes).unwrap();
        let artifact = Artifact {
            schema: 1,
            version: "0.100.0".into(),
            build_number: 100,
            commit: "a".repeat(40),
            target: target.triple().into(),
            asset: asset.clone(),
            sha256: sha256_file(&dir.join(&asset)).unwrap(),
            size: bytes.len() as u64,
            source_fingerprint: "b".repeat(64),
            compiler_sha256: "c".repeat(64),
            driver_sha256: "d".repeat(64),
            compiler_library_sha256: "c".repeat(64),
            compiler_provenance_sha256: "e".repeat(64),
            compiler_version: format!("binary: trustc\nhost: {}\ntrust: 0.1.0\n", target.triple()),
        };
        fs::write(
            dir.join(format!("{asset}.handoff.toml")),
            aterm_toml::to_string(&artifact).unwrap(),
        )
        .unwrap();
        artifact
    }

    fn manifest() -> Manifest {
        Manifest::parse(&format!("schema=1\nversion=\"0.100.0\"\nbuild_number=100\ncommit=\"{}\"\ndmg=\"aterm-0.100.0.dmg\"\nsha256=\"{}\"\nteam_id=\"\"\n", "a".repeat(40), "f".repeat(64))).unwrap()
    }

    #[test]
    fn imports_both_architectures_and_binds_signed_manifest_fields() {
        let source = Scratch::new();
        let dist = Scratch::new();
        for t in TARGETS {
            fixture(&source.0, t);
        }
        let mut handoff = Handoff::new(&source.0, &[], &[]).unwrap();
        handoff
            .import(&dist.0, "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        handoff
            .verify_staged(&dist.0, "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        let mut manifest = manifest();
        handoff.stamp(&mut manifest).unwrap();
        assert_eq!(manifest_asset_names(&manifest).unwrap().len(), 2);
        verify_manifest_handoff(&manifest, Some(&handoff)).unwrap();
        assert!(verify_manifest_handoff(&manifest, None).is_err());
        manifest.linux_aarch64_sha256 = Some("f".repeat(64));
        assert!(verify_manifest_handoff(&manifest, Some(&handoff)).is_err());
    }

    #[test]
    fn explicit_native_subset_is_honest_and_default_refuses_missing_worker() {
        let source = Scratch::new();
        let dist = Scratch::new();
        fixture(&source.0, LinuxTarget::Aarch64);
        assert!(
            Handoff::new(&source.0, &[], &[])
                .unwrap()
                .import(&dist.0, "0.100.0", 100, &"a".repeat(40))
                .is_err()
        );
        let mut handoff =
            Handoff::new(&source.0, &[LinuxTarget::Aarch64.triple().into()], &[]).unwrap();
        handoff
            .import(&dist.0, "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        let mut manifest = manifest();
        handoff.stamp(&mut manifest).unwrap();
        assert!(manifest.linux_x86_64.is_none());
        assert!(manifest.linux_aarch64.is_some());
        assert!(Handoff::new(&source.0, &["riscv64".into()], &[]).is_err());
        assert!(
            Handoff::new(
                &source.0,
                &[
                    LinuxTarget::Aarch64.triple().into(),
                    LinuxTarget::Aarch64.triple().into()
                ],
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn resume_refuses_changed_provenance_and_tampered_staged_bytes() {
        let source = Scratch::new();
        let dist = Scratch::new();
        let mut artifact = fixture(&source.0, LinuxTarget::Aarch64);
        let mut handoff =
            Handoff::new(&source.0, std::slice::from_ref(&artifact.target), &[]).unwrap();
        handoff
            .import(&dist.0, "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        let frozen: Handoff =
            aterm_toml::from_str(&aterm_toml::to_string(&handoff).unwrap()).unwrap();
        assert_eq!(frozen, handoff);
        artifact.compiler_sha256 = "f".repeat(64);
        fs::write(
            source.0.join(format!("{}.handoff.toml", artifact.asset)),
            aterm_toml::to_string(&artifact).unwrap(),
        )
        .unwrap();
        assert!(
            handoff
                .import(&dist.0, "0.100.0", 100, &"a".repeat(40))
                .unwrap_err()
                .contains("changed")
        );
        let mut data = fs::read(dist.0.join(&artifact.asset)).unwrap();
        data[70] ^= 1;
        fs::write(dist.0.join(&artifact.asset), data).unwrap();
        assert!(
            frozen
                .verify_staged(&dist.0, "0.100.0", 100, &"a".repeat(40))
                .unwrap_err()
                .contains("hash")
        );
    }

    #[test]
    fn wrong_build_commit_arch_and_symlink_are_refused() {
        let source = Scratch::new();
        let dist = Scratch::new();
        let mut artifact = fixture(&source.0, LinuxTarget::Aarch64);
        assert!(artifact.validate("0.100.0", 101, &"a".repeat(40)).is_err());
        assert!(artifact.validate("0.100.0", 100, &"b".repeat(40)).is_err());
        artifact.target = LinuxTarget::X86_64.triple().into();
        assert!(
            artifact
                .verify_file(&source.0.join(&artifact.asset))
                .is_err()
        );
        let alias = dist.0.join("alias");
        std::os::unix::fs::symlink(source.0.join(&artifact.asset), &alias).unwrap();
        assert!(artifact.verify_file(&alias).is_err());
        assert!(check_identity("0.100.0", 0, &"a".repeat(40)).is_err());
        assert!(check_identity("0.100.0", 1, "short").is_err());
    }

    #[test]
    fn the_channel_release_requires_every_signed_raw_asset_exactly_once() {
        let source = Scratch::new();
        let artifact = fixture(&source.0, LinuxTarget::Aarch64);
        let mut published = crate::channel::required_asset_names("0.100.0", false, false);
        assert!(
            crate::channel::validate_channel_asset_set(
                &published,
                "0.100.0",
                false,
                false,
                std::slice::from_ref(&artifact.asset),
            )
            .is_err(),
            "a declared native executable the release does not carry is missing"
        );
        published.push(artifact.asset.clone());
        crate::channel::validate_channel_asset_set(
            &published,
            "0.100.0",
            false,
            false,
            std::slice::from_ref(&artifact.asset),
        )
        .unwrap();
        published.push(artifact.asset.clone());
        assert!(
            crate::channel::validate_channel_asset_set(
                &published,
                "0.100.0",
                false,
                false,
                &[artifact.asset],
            )
            .is_err(),
            "a duplicated native executable is refused"
        );
    }

    #[test]
    fn worker_and_cut_cli_reject_ambiguous_targets_and_allow_local_only_builds() {
        let parse = |args: &[&str]| {
            crate::cli::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        assert!(matches!(
            parse(&[
                "linux-build",
                "--version",
                "0.100.0",
                "--build-number",
                "100",
                "--commit",
                &"a".repeat(40),
                "--out",
                "/private/output"
            ])
            .unwrap(),
            crate::cli::Cmd::LinuxBuild { .. }
        ));
        assert!(
            parse(&[
                "linux-build",
                "--version",
                "0.100.0",
                "--build-number",
                "0",
                "--commit",
                &"a".repeat(40),
                "--out",
                "/private/output"
            ])
            .is_err()
        );
        assert!(parse(&["cut", "--linux-target", "aarch64"]).is_err());
        assert!(
            parse(&[
                "cut",
                "--linux-artifacts",
                "/private/output",
                "--linux-target",
                "riscv64"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "cut",
                "--linux-artifacts",
                "/private/output",
                "--linux-target",
                "aarch64",
                "--linux-target",
                "aarch64"
            ])
            .is_err()
        );
        assert!(parse(&["cut", "--resume", "--linux-artifacts", "/changed/output"]).is_err());
        // Linux is declared, never defaulted away: a real or rehearsal cut that
        // names neither --linux-artifacts nor --mac-only is refused pre-claim.
        let silent = parse(&["cut"]).unwrap_err();
        assert_eq!(silent, crate::cli::MAC_ONLY_REFUSAL);
        assert!(parse(&["cut", "--rehearse", "o/r"]).is_err());
        assert!(parse(&["cut", "--mac-only"]).is_ok());
        assert!(parse(&["cut", "--linux-artifacts", "/private/output"]).is_ok());
        assert!(
            parse(&["cut", "--dry-run"]).is_ok(),
            "a dry run publishes nothing"
        );
        assert!(
            parse(&["cut", "--resume"]).is_ok(),
            "a resume has its shape already"
        );
        assert!(
            parse(&["cut", "--mac-only", "--linux-artifacts", "/private/output"]).is_err(),
            "contradiction"
        );
        assert!(parse(&["cut", "--resume", "--mac-only"]).is_err());
        assert!(parse(&["cut", "--abandon", "v0.26.0", "--mac-only"]).is_err());
        // A cutter handed its cut by another is exempt: its parent held the rule.
        assert!(
            crate::cli::parse_handed_off(&["cut".to_string()]).is_ok(),
            "the handed-off cutter reads its parent's argv"
        );
        // --linux-worker needs somewhere to land and a well-formed spec.
        assert!(parse(&["cut", "--mac-only", "--linux-worker", "aarch64=h:/r"]).is_err());
        assert!(
            parse(&[
                "cut",
                "--linux-artifacts",
                "/private/output",
                "--linux-worker",
                "aarch64=builder@buildhost:~/aterm"
            ])
            .is_ok()
        );
        assert!(
            parse(&[
                "cut",
                "--linux-artifacts",
                "/private/output",
                "--linux-worker",
                "aarch64=-oProxyCommand=x:/r"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "cut",
                "--dry-run",
                "--linux-artifacts",
                "/private/output",
                "--linux-target",
                "aarch64"
            ])
            .is_ok()
        );
    }

    #[test]
    fn a_worker_spec_is_one_shell_safe_host_and_path() {
        let worker = Worker::parse("aarch64=builder@buildhost:/home/builder/src/aterm").unwrap();
        assert_eq!(worker.target, LinuxTarget::Aarch64.triple());
        assert_eq!(worker.destination, "builder@buildhost");
        assert_eq!(worker.repository, "/home/builder/src/aterm");
        assert_eq!(
            Worker::parse("x86_64=box:~/src/aterm").unwrap().target,
            LinuxTarget::X86_64.triple()
        );
        for bad in [
            "aarch64",
            "aarch64=host",
            "riscv64=host:/r",
            "aarch64=:/r",
            "aarch64=-oProxyCommand=evil:/r",
            "aarch64=host;rm:/r",
            "aarch64=host:relative/path",
            "aarch64=host:/r/../etc",
            "aarch64=host:/r with space",
            "aarch64=host:/r$(id)",
            "aarch64=host:~root/r",
        ] {
            assert!(Worker::parse(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn workers_must_build_a_declared_target_once() {
        let dir = Scratch::new();
        let aarch64 = LinuxTarget::Aarch64.triple().to_string();
        let handoff = Handoff::new(
            &dir.0,
            std::slice::from_ref(&aarch64),
            &["aarch64=h:/r".into()],
        )
        .unwrap();
        assert_eq!(handoff.workers.len(), 1);
        assert!(
            Handoff::new(
                &dir.0,
                std::slice::from_ref(&aarch64),
                &["x86_64=h:/r".into()]
            )
            .is_err(),
            "a worker for an architecture the cut does not declare"
        );
        assert!(
            Handoff::new(&dir.0, &[], &["aarch64=a:/r".into(), "aarch64=b:/r".into()]).is_err(),
            "two workers for one architecture"
        );
    }

    #[test]
    fn only_this_cuts_valid_pair_counts_as_already_built() {
        let dir = Scratch::new();
        let target = LinuxTarget::Aarch64;
        let commit = "a".repeat(40);
        assert!(
            !pair_is_current(&dir.0, target, "0.100.0", 100, &commit),
            "nothing there"
        );
        fixture(&dir.0, target);
        assert!(pair_is_current(&dir.0, target, "0.100.0", 100, &commit));
        // A dry run's (or a failed claim's) pair: same file names, other build.
        assert!(!pair_is_current(&dir.0, target, "0.100.0", 101, &commit));
        assert!(!pair_is_current(
            &dir.0,
            target,
            "0.100.0",
            100,
            &"b".repeat(40)
        ));
        assert!(!pair_is_current(
            &dir.0,
            LinuxTarget::X86_64,
            "0.100.0",
            100,
            &commit
        ));
        // A torn copy: the executable no longer matches its receipt.
        let asset = dir.0.join(target.asset_name("0.100.0"));
        let mut bytes = fs::read(&asset).unwrap();
        bytes.truncate(70);
        fs::write(&asset, bytes).unwrap();
        assert!(!pair_is_current(&dir.0, target, "0.100.0", 100, &commit));
    }

    #[test]
    fn the_worker_script_is_parsed_whole_before_it_runs() {
        // One function, called with stdin from /dev/null: nothing the build runs
        // can read the rest of the script off ssh's pipe.
        assert!(WORKER_SCRIPT.starts_with("main() {\n"));
        assert!(WORKER_SCRIPT.trim_end().ends_with("main \"$@\" </dev/null"));
        assert!(WORKER_SCRIPT.contains("umask 077"));
        assert!(WORKER_SCRIPT.contains("mktemp -d"));
        if std::path::Path::new("/bin/bash").exists() {
            let status = Command::new("/bin/bash")
                .args(["-n", "-c", WORKER_SCRIPT])
                .status()
                .unwrap();
            assert!(status.success(), "bash -n rejects the worker script");
        }
    }
}
