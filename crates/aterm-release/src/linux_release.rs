// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Linux worker and immutable handoff to the one release cutter.

use aterm_uds::spawnfd::{Command, command};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use aterm_bootstrap_verify::pin::{self, Pin};
use aterm_update_core::linux::{LINUX_BINARY_MAX_BYTES, LinuxTarget};
use aterm_update_core::{Manifest, sha256_file};
use serde::{Deserialize, Serialize};

#[path = "pin_release.rs"]
pub mod from_release;

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

/// The static bootstrap verifier a worker built beside the executable
/// (crates/aterm-bootstrap-verify): the release asset install.sh fetches and holds to the
/// SHA-256 it pins ([`pin::Pin`]). install.sh's pin is its trust anchor at install time, so a
/// release without one changes nothing but leaving install.sh on its embedded verifier for
/// that architecture. The signed appcast carries its name, digest and size
/// ([`Handoff::stamp`]), which no client reads: they are what binds the pin to the release
/// signer when it is read back from the public release ([`from_release`]). It is built by
/// upstream stable, static (the STOCK EXCEPTION, `buildplan::build_static`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierArtifact {
    pub schema: u32,
    pub version: String,
    pub build_number: u64,
    pub commit: String,
    pub target: String,
    pub asset: String,
    pub sha256: String,
    pub size: u64,
    pub source_fingerprint: String,
    /// Stable's `rustc -vV`, the compiler that built it.
    pub compiler_version: String,
}

/// The ELF program header type of a dynamic loader's path.
const PT_INTERP: u32 = 3;

/// Whether `elf`, an ELF64 little-endian file, names no program interpreter: the kernel
/// runs it with no dynamic loader, on any C library or none. A program header table that
/// does not fit the file is no answer.
fn names_no_interpreter(elf: &[u8]) -> bool {
    let field = |at: usize, len: usize| -> Option<u64> {
        let bytes = elf.get(at..at.checked_add(len)?)?;
        Some(
            bytes
                .iter()
                .rev()
                .fold(0, |value, byte| (value << 8) | u64::from(*byte)),
        )
    };
    let (Some(offset), Some(size), Some(count)) = (field(32, 8), field(54, 2), field(56, 2)) else {
        return false;
    };
    if count > 0 && size < 4 {
        return false;
    }
    (0..count).all(|n| {
        let at = n
            .checked_mul(size)
            .and_then(|step| step.checked_add(offset))
            .and_then(|at| usize::try_from(at).ok());
        at.and_then(|at| field(at, 4))
            .is_some_and(|kind| kind != u64::from(PT_INTERP))
    })
}

impl VerifierArtifact {
    /// The receipt is this cut's: its version, build and commit, its architecture's asset
    /// name, a size install.sh downloads, and stable's identity for its host.
    pub fn validate(&self, version: &str, build: u64, commit: &str) -> Result<(), String> {
        check_identity(version, build, commit)?;
        let target = target(&self.target)?;
        if self.schema != 1
            || self.version != version
            || self.build_number != build
            || self.commit != commit
            || self.asset != pin::asset_name(version, target.arch())
            || !(64..=pin::MAX_ASSET).contains(&self.size)
            || !aterm_codec::hex::is_lower(&self.sha256, 64)
            || !aterm_codec::hex::is_lower(&self.source_fingerprint, 64)
            || !self
                .compiler_version
                .lines()
                .any(|line| line.starts_with("release: "))
            || !self
                .compiler_version
                .lines()
                .any(|line| line == format!("host: {}", target.triple()))
        {
            return Err(
                "Linux bootstrap verifier receipt has mismatched source/build/target or no \
                 compiler identity"
                    .into(),
            );
        }
        Ok(())
    }

    /// The file at `path` is the receipt's: a regular file of its size, this
    /// architecture's static ELF, of its SHA-256.
    pub fn verify_file(&self, path: &Path) -> Result<(), String> {
        static_verifier_file(path, target(&self.target)?, self.size, &self.sha256)
    }
}

/// The file at `path` is `target`'s static bootstrap verifier of `size` bytes and SHA-256
/// `sha256`: a regular file of that size, that architecture's ELF naming no program
/// interpreter, of that digest.
///
/// # Errors
/// Any of those that it is not, or a file it cannot inspect or read.
pub fn static_verifier_file(
    path: &Path,
    target: LinuxTarget,
    size: u64,
    sha256: &str,
) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("inspect {}: {e}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.len() != size {
        return Err(format!(
            "Linux bootstrap verifier is not a regular file of the claimed size: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(pin::MAX_ASSET + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    target.validate_elf_header(bytes.get(..64).unwrap_or(&bytes))?;
    if !names_no_interpreter(&bytes) {
        return Err(format!(
            "Linux bootstrap verifier is not a static executable (it names a program \
             interpreter): {}",
            path.display()
        ));
    }
    let mut digest = aterm_digest::Sha256::new();
    digest.update(&bytes);
    if aterm_codec::hex::encode_lower(&digest.finalize()) != sha256 {
        return Err(format!(
            "Linux bootstrap verifier hash changed: {}",
            path.display()
        ));
    }
    Ok(())
}

/// The verifier fields of `manifest` for `target`: name, SHA-256, size.
fn verifier_fields(
    manifest: &Manifest,
    target: LinuxTarget,
) -> (Option<&str>, Option<&str>, Option<u64>) {
    match target {
        LinuxTarget::X86_64 => (
            manifest.linux_verifier_x86_64.as_deref(),
            manifest.linux_verifier_x86_64_sha256.as_deref(),
            manifest.linux_verifier_x86_64_size,
        ),
        LinuxTarget::Aarch64 => (
            manifest.linux_verifier_aarch64.as_deref(),
            manifest.linux_verifier_aarch64_sha256.as_deref(),
            manifest.linux_verifier_aarch64_size,
        ),
    }
}

/// `manifest` with no verifier fields.
fn without_verifiers(manifest: &mut Manifest) {
    manifest.linux_verifier_x86_64 = None;
    manifest.linux_verifier_x86_64_sha256 = None;
    manifest.linux_verifier_x86_64_size = None;
    manifest.linux_verifier_aarch64 = None;
    manifest.linux_verifier_aarch64_sha256 = None;
    manifest.linux_verifier_aarch64_size = None;
    manifest.linux_verifier_interface = None;
}

/// The bootstrap verifiers a signed `manifest` names ([`Handoff::stamp`]), in [`TARGETS`]
/// order, each a [`pin::Receipt`] of a canonical asset name for the manifest's version, and
/// the interface they speak; `None` when it names none.
///
/// # Errors
/// An architecture with some of its three fields and not the others, a noncanonical name, a
/// digest that is not 64 lowercase hex digits, a size install.sh would not download, or
/// receipts with no interface (or an interface with no receipt).
pub fn signed_verifiers(manifest: &Manifest) -> Result<Option<(Vec<pin::Receipt>, u32)>, String> {
    let mut receipts = Vec::new();
    for target in TARGETS {
        let arch = target.arch();
        match verifier_fields(manifest, target) {
            (None, None, None) => {}
            (Some(asset), Some(sha256), Some(size)) => {
                if asset != pin::asset_name(&manifest.version, arch) {
                    return Err(format!(
                        "the signed appcast names the {arch} verifier {asset:?}, not v{}'s \
                         canonical asset",
                        manifest.version
                    ));
                }
                if !aterm_codec::hex::is_lower(sha256, 64) {
                    return Err(format!(
                        "the signed appcast's {arch} verifier digest {sha256:?} is not 64 \
                         lowercase hex digits"
                    ));
                }
                if !(64..=pin::MAX_ASSET).contains(&size) {
                    return Err(format!(
                        "the signed appcast's {arch} verifier is {size} bytes, outside \
                         64..={}",
                        pin::MAX_ASSET
                    ));
                }
                receipts.push(pin::Receipt {
                    arch: arch.to_string(),
                    asset: asset.to_string(),
                    sha256: sha256.to_string(),
                    size,
                });
            }
            _ => {
                return Err(format!(
                    "the signed appcast's {arch} verifier is incomplete: its name, sha256 and \
                     size appear together or not at all"
                ));
            }
        }
    }
    match (receipts.is_empty(), manifest.linux_verifier_interface) {
        (true, None) => Ok(None),
        (false, Some(interface)) => Ok(Some((receipts, interface))),
        (true, Some(_)) => {
            Err("the signed appcast names a verifier interface but no verifier".into())
        }
        (false, None) => Err("the signed appcast names verifiers but no interface".into()),
    }
}

/// The receipts a cut writes into its dist/ beside its other files when it froze a static
/// verifier ([`Handoff::record`]): what `ship pin-verifier` writes install.sh's pin from.
pub const PIN_RECORD: &str = "aterm-bootstrap-verify-pin.toml";

/// `targo --unverified ship pin-verifier <record>`, once the release a cut's [`PIN_RECORD`]
/// names is public: write it into the checkout at `root` as tools/install.sh's pin and the
/// committed record beside it ([`pin::RECORD`]), the two `pin::check` holds to each other.
/// The pin it wrote.
///
/// This path trusts the record file itself: the cutting machine's `dist/`, whose receipts
/// its own sign step froze from the files it uploaded. Nothing here re-reads the release;
/// from any other checkout, [`from_release::pin_from_release`] reads the public release and
/// binds the digests to its signer instead.
///
/// # Errors
/// A record of another shape or interface, one this checkout's RELEASES.ledger does not
/// claim, or an install.sh whose pin it cannot place: nothing is written.
pub fn pin_verifier(root: &Path, record: &Path) -> Result<Pin, String> {
    let text = fs::read_to_string(record).map_err(|e| format!("{}: {e}", record.display()))?;
    write_pin(root, &pin::Record::parse(&text)?)
}

/// Write `record` into the checkout at `root` as tools/install.sh's pin and the committed
/// record beside it ([`pin::RECORD`]), once both pass `pin::check`. The pin it wrote.
///
/// # Errors
/// A record this checkout's RELEASES.ledger does not claim, one of another interface, or an
/// install.sh whose pin it cannot place: nothing is written.
pub fn write_pin(root: &Path, record: &pin::Record) -> Result<Pin, String> {
    let read =
        |path: &Path| fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()));
    record.validate()?;
    let ledger = read(&root.join("RELEASES.ledger"))?;
    record.claimed_in(&ledger).map_err(|why| {
        format!(
            "{why}. Pin from a checkout of main that holds the release's claim (pull it into {} \
             first)",
            root.display()
        )
    })?;
    let install = root.join("tools/install.sh");
    let script = record.pin().rewrite(&read(&install)?)?;
    let body = record.to_toml();
    let pinned = pin::check(&script, Some(&body), &ledger)?;
    fs::write(&install, script).map_err(|e| format!("{}: {e}", install.display()))?;
    let at = root.join(pin::RECORD);
    fs::write(&at, body).map_err(|e| format!("{}: {e}", at.display()))?;
    Ok(pinned)
}

/// The directory is fixed before the claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub directory: PathBuf,
    pub targets: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// The static bootstrap verifiers beside the artifacts, in target order: optional per
    /// architecture (one a worker could not build stays on install.sh's embedded
    /// verifier), and frozen with the artifacts.
    #[serde(default)]
    pub verifiers: Vec<VerifierArtifact>,
    /// The native hosts the cutter drives itself (`--linux-worker`), journaled so a resumed cut
    /// still knows where each architecture is built.
    #[serde(default)]
    pub workers: Vec<Worker>,
}

/// A native Linux host the cutter drives over ssh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    /// The Linux target triple this host builds natively.
    pub target: String,
    /// The ssh destination (`host` or `user@host`, an ssh config alias included).
    pub destination: String,
    /// The aterm checkout on that host that the throwaway worktree comes from: absolute, or
    /// `~/`-relative to the remote login.
    pub repository: String,
}

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

fn shell_word(value: &str, extra: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c) || extra.contains(c))
}

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

    fn run(&self, directory: &Path, version: &str, build: u64, commit: &str) -> Result<(), String> {
        let target = target(&self.target)?;
        check_identity(version, build, commit)?;
        let name = target.asset_name(version);
        let receipt = format!("{name}.handoff.toml");
        let verifier = pin::asset_name(version, target.arch());
        let verifier_receipt = format!("{verifier}.handoff.toml");
        if pair_is_current(directory, target, version, build, commit) {
            println!(
                "==> Linux worker {}: this cut's pair is already in {}; not rebuilding",
                target.triple(),
                directory.display()
            );
            return Ok(());
        }
        for stale in [&name, &receipt, &verifier, &verifier_receipt] {
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
        let mut child = command("ssh")
            .args(SSH_OPTIONS)
            .args(["-a", "-x", "--", &self.destination])
            .args(["bash", "-l", "-s", "--", &self.repository, commit, version])
            .arg(build.to_string())
            .arg(arch)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
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
        let staging = directory.join(format!(".worker-{arch}"));
        match fs::remove_dir_all(&staging) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("clear {}: {e}", staging.display())),
        }
        fs::create_dir(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;
        let status = command("scp")
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
        // The static bootstrap verifier's pair, when the worker built one: best effort,
        // since a release without it leaves install.sh on its embedded verifier for this
        // architecture. Receipt last, as above.
        let copied = command("scp")
            .args(SSH_OPTIONS)
            .args(["-q", "--"])
            .arg(format!("{}:{remote_out}/{verifier}", self.destination))
            .arg(format!(
                "{}:{remote_out}/{verifier_receipt}",
                self.destination
            ))
            .arg(&staging)
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
            && [&verifier, &verifier_receipt]
                .into_iter()
                .all(|file| fs::rename(staging.join(file), directory.join(file)).is_ok());
        if !copied {
            for file in [&verifier, &verifier_receipt] {
                let _ = fs::remove_file(directory.join(file));
                let _ = fs::remove_file(staging.join(file));
            }
            println!(
                "==> Linux worker {}: no static bootstrap verifier came back; this release \
                 publishes none for it",
                target.triple()
            );
        }
        let _ = fs::remove_dir(&staging);
        let _ = command("ssh")
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
            .stdin(aterm_uds::spawnfd::Stdio::null())
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

/// The Linux assets `manifest` signs: each executable, then each static verifier.
///
/// # Errors
/// An executable's or a verifier's fields the manifest's readers refuse.
pub fn manifest_asset_names(manifest: &Manifest) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for target in TARGETS {
        if let Some(artifact) = manifest.linux_artifact(target)? {
            names.push(artifact.name.to_string());
        }
    }
    if let Some((receipts, _)) = signed_verifiers(manifest)? {
        names.extend(receipts.into_iter().map(|receipt| receipt.asset));
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
    without_verifiers(&mut expected);
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
    if signed_verifiers(manifest)? != signed_verifiers(&expected)? {
        return Err(
            "the signed bootstrap verifier fields disagree with the cut's frozen worker handoff \
             (a cut whose appcast a cutter from before those fields signed is finished by the \
             cutter that started it, or abandoned)"
                .into(),
        );
    }
    Ok(())
}

fn target(value: &str) -> Result<LinuxTarget, String> {
    TARGETS
        .into_iter()
        .find(|t| t.triple() == value)
        .ok_or_else(|| format!("unsupported Linux handoff target {value:?}"))
}

pub fn check_identity(version: &str, build: u64, commit: &str) -> Result<(), String> {
    crate::ledger::check_version_shape(version).map_err(|e| e.to_string())?;
    if build == 0 || !aterm_codec::hex::is_lower(commit, 40) {
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
            .all(|v| aterm_codec::hex::is_lower(v, 64))
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
        for verifier in &self.verifiers {
            verifier.validate(version, build, commit)?;
        }
        let order: Vec<Option<usize>> = self
            .verifiers
            .iter()
            .map(|verifier| self.targets.iter().position(|t| *t == verifier.target))
            .collect();
        if order.iter().any(Option::is_none) || order.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(
                "Linux handoff's bootstrap verifiers are not in its declared target order".into(),
            );
        }
        Ok(())
    }

    /// The receipts install.sh's pin is written from ([`pin::Record`]): this cut's
    /// verifiers, published on `repo` as release `version` (build `build`, commit
    /// `commit`). None when it froze no verifier.
    #[must_use]
    pub fn record(
        &self,
        repo: &str,
        version: &str,
        build: u64,
        commit: &str,
    ) -> Option<pin::Record> {
        if self.verifiers.is_empty() {
            return None;
        }
        let verifiers = TARGETS
            .iter()
            .filter_map(|target| {
                self.verifiers
                    .iter()
                    .find(|verifier| verifier.target == target.triple())
                    .map(|verifier| pin::Receipt {
                        arch: target.arch().to_string(),
                        asset: verifier.asset.clone(),
                        sha256: verifier.sha256.clone(),
                        size: verifier.size,
                    })
            })
            .collect();
        Some(pin::Record {
            repo: repo.to_string(),
            tag: format!("v{version}"),
            build_number: build,
            commit: commit.to_string(),
            interface: pin::INTERFACE,
            verifiers,
        })
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
            verifiers: Vec::new(),
            workers,
        })
    }

    /// Run every declared [`Worker`] whose pair is not in the handoff directory yet.
    pub fn run_workers(&self, version: &str, build: u64, commit: &str) -> Result<(), String> {
        private_directory(&self.directory)?;
        for worker in &self.workers {
            worker.run(&self.directory, version, build, commit)?;
        }
        Ok(())
    }

    /// Import every declared architecture's pair (all are required) into `dist`.
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
        let verifiers = self.verifier_pairs(version, build, commit)?;
        if !self.artifacts.is_empty() && (self.artifacts != found || self.verifiers != verifiers) {
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
        for verifier in &verifiers {
            atomic_copy(
                &self.directory.join(&verifier.asset),
                &dist.join(&verifier.asset),
            )?;
            verifier.verify_file(&dist.join(&verifier.asset))?;
        }
        self.artifacts = found;
        self.verifiers = verifiers;
        Ok(())
    }

    /// Each declared architecture's bootstrap verifier pair in the handoff directory, in
    /// target order. One with no receipt is absent: that architecture's release publishes
    /// none, and install.sh keeps its embedded verifier there. A receipt that is there must
    /// be this cut's and its file the receipt's.
    fn verifier_pairs(
        &self,
        version: &str,
        build: u64,
        commit: &str,
    ) -> Result<Vec<VerifierArtifact>, String> {
        let mut found = Vec::new();
        for requested in &self.targets {
            let target = target(requested)?;
            let name = pin::asset_name(version, target.arch());
            let receipt_path = self.directory.join(format!("{name}.handoff.toml"));
            match fs::symlink_metadata(&receipt_path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    println!(
                        "==> no static bootstrap verifier for {}: this release publishes none \
                         for it, and install.sh keeps its embedded verifier there",
                        target.triple()
                    );
                    continue;
                }
                Err(e) => return Err(format!("inspect {}: {e}", receipt_path.display())),
                Ok(metadata) if !metadata.file_type().is_file() || metadata.len() > 65536 => {
                    return Err("unsafe or oversized Linux bootstrap verifier receipt".into());
                }
                Ok(_) => {}
            }
            let verifier: VerifierArtifact = aterm_toml::from_str(
                &fs::read_to_string(&receipt_path).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("parse Linux bootstrap verifier receipt: {e}"))?;
            verifier.validate(version, build, commit)?;
            if verifier.target != target.triple() {
                return Err(
                    "Linux bootstrap verifier receipt's architecture does not match its filename"
                        .into(),
                );
            }
            verifier.verify_file(&self.directory.join(&name))?;
            found.push(verifier);
        }
        Ok(found)
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
        for verifier in &self.verifiers {
            verifier.verify_file(&dist.join(&verifier.asset))?;
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
        // The static verifiers go into the same signed bytes: no client reads them, and they
        // are what `ship pin-verifier --from-release` binds install.sh's pin to.
        for verifier in &self.verifiers {
            verifier.validate(
                &manifest.version,
                manifest.build_number,
                manifest
                    .commit
                    .as_deref()
                    .ok_or("Linux manifest requires a source commit")?,
            )?;
            let fields = (
                Some(verifier.asset.clone()),
                Some(verifier.sha256.clone()),
                Some(verifier.size),
            );
            match target(&verifier.target)? {
                LinuxTarget::X86_64 => {
                    (
                        manifest.linux_verifier_x86_64,
                        manifest.linux_verifier_x86_64_sha256,
                        manifest.linux_verifier_x86_64_size,
                    ) = fields;
                }
                LinuxTarget::Aarch64 => {
                    (
                        manifest.linux_verifier_aarch64,
                        manifest.linux_verifier_aarch64_sha256,
                        manifest.linux_verifier_aarch64_size,
                    ) = fields;
                }
            }
            manifest.linux_verifier_interface = Some(pin::INTERFACE);
        }
        Ok(())
    }
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let uid = super::current_release_uid()?;
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
        crate::durable::sync_all(&output).map_err(|e| e.to_string())?;
        fs::rename(&temp, destination).map_err(|e| e.to_string())?;
        crate::durable::sync_dir(destination.parent().ok_or("Linux artifact has no parent")?)
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
        command(aterm_uds::os_path!("git"))
            .args(["rev-parse", "HEAD"])
            .current_dir(repo),
        "Linux worker source HEAD",
    )?;
    let dirty = crate::gates::dirty_paths(repo).map_err(|e| e.to_string())?;
    if head.trim() != commit || !dirty.is_empty() {
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

/// A local-only worker: no credentials, signing, release API or upload code is reachable from here.
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
    crate::gates::own_cutter_gate(&git, commit, false).map_err(|e| e.to_string())?;
    private_directory(out)?;
    let name = native.asset_name(version);
    let receipt = out.join(format!("{name}.handoff.toml"));
    let verifier = pin::asset_name(version, native.arch());
    let outputs = [
        out.join(&name),
        receipt.clone(),
        out.join(&verifier),
        out.join(format!("{verifier}.handoff.toml")),
    ];
    if outputs.iter().any(|path| path.exists()) {
        return Err("Linux output already exists; choose a fresh handoff directory (never overwrite provenance)".into());
    }
    let compiler_dir = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
    let compiler = compiler_dir.join("trustc");
    let driver = compiler_dir.join("targo");
    let provenance = compiler_dir
        .parent()
        .ok_or("Trust seal has no root")?
        .join("PROVENANCE");
    let compiler_version = output_text(command(&compiler).arg("-vV"), "Trust compiler identity")?;
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
    let take = super::Take::prepare(repo, commit)?;
    let plan = super::BuildPlan {
        repo_root: repo.into(),
        commit: commit.into(),
        out_dir: out.into(),
        build_number: build,
        short_version: version.into(),
        expected_update_pin_sha256: None,
        one_slice_at_a_time: true,
        index_hint: super::index_hint_now(),
    };
    super::build_one(&plan, &take, "aterm", None, None)?;
    take.verify()?;
    clean_head(repo, commit)?;
    if sha256_file(&compiler)? != compiler_hash
        || sha256_file(&driver)? != driver_hash
        || sha256_file(&provenance)? != provenance_hash
        || sha256_file(&compiler_library)? != library_hash
    {
        return Err("Trust seal changed during the native Linux build".into());
    }
    let binary = take.slice_target(None).join("release/aterm");
    let version_output = command(&binary)
        .arg("--version")
        .output()
        .map_err(|e| e.to_string())?;
    if !version_output.status.success() {
        return Err("native Linux binary cannot start".into());
    }
    super::validate_cli_app_version(version, &version_output.stdout)?;
    let identity = output_text(
        command(&binary).args(["update", "identity"]),
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
            "Linux executable does not report the exact clean source/build/target identity".into(),
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
        source_fingerprint: take.snapshot.digest.clone(),
        compiler_sha256: compiler_hash,
        driver_sha256: driver_hash,
        compiler_library_sha256: library_hash,
        compiler_version,
        compiler_provenance_sha256: provenance_hash,
    };
    artifact.validate(version, build, commit)?;
    artifact.verify_file(&binary)?;
    atomic_copy(&binary, &out.join(&name))?;
    write_receipt(
        &receipt,
        &aterm_toml::to_string(&artifact).map_err(|e| e.to_string())?,
    )?;
    println!(
        "Linux local handoff ready: {} (unsigned; only ship cut may publish it)",
        out.join(&name).display()
    );
    // The static bootstrap verifier beside it, from the same take. Soft: a host that
    // cannot build it (no stable, no static C library) leaves this architecture's release
    // without one, and install.sh keeps its embedded verifier there.
    match build_bootstrap_verifier(&plan, &take, native, out, version, build, commit) {
        Ok(path) => println!(
            "Linux bootstrap verifier ready: {} (unsigned; install.sh pins it by SHA-256)",
            path.display()
        ),
        Err(why) => println!(
            "==> no static bootstrap verifier for {}: {why}. This release publishes none for \
             it, and install.sh keeps its embedded verifier there",
            native.triple()
        ),
    }
    take.verify()?;
    clean_head(repo, commit)?;
    Ok(())
}

/// A receipt at `path`, written once and synced.
fn write_receipt(path: &Path, text: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|()| crate::durable::sync_all(&file))
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// Build this host's static bootstrap verifier from `take` and put its pair in `out`:
/// upstream stable, static (the STOCK EXCEPTION, `buildplan::build_static`), held to what
/// install.sh runs: this architecture's static ELF, which refuses an invocation it cannot
/// read in install.sh's words.
fn build_bootstrap_verifier(
    plan: &super::BuildPlan,
    take: &super::Take,
    native: LinuxTarget,
    out: &Path,
    version: &str,
    build: u64,
    commit: &str,
) -> Result<PathBuf, String> {
    let binary = super::build_static(
        plan,
        take,
        "aterm-bootstrap-verify",
        "aterm-bootstrap-verify",
        native.triple(),
        None,
    )?;
    take.verify()?;
    let said = command(&binary)
        .env_clear()
        .current_dir("/")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| format!("run {}: {e}", binary.display()))?;
    let refusal = format!(
        "{}invalid Linux verifier invocation\n",
        aterm_bootstrap_verify::REFUSED
    );
    if said.status.code() != Some(1) || !said.stdout.is_empty() || said.stderr != refusal.as_bytes()
    {
        return Err(format!(
            "the built verifier does not refuse a bare invocation as install.sh reads one \
             ({}: {:?})",
            said.status,
            String::from_utf8_lossy(&said.stderr)
        ));
    }
    let name = pin::asset_name(version, native.arch());
    let verifier = VerifierArtifact {
        schema: 1,
        version: version.into(),
        build_number: build,
        commit: commit.into(),
        target: native.triple().into(),
        asset: name.clone(),
        sha256: sha256_file(&binary)?,
        size: fs::metadata(&binary).map_err(|e| e.to_string())?.len(),
        source_fingerprint: take.snapshot.digest.clone(),
        compiler_version: super::stable_rustc_version()?,
    };
    verifier.validate(version, build, commit)?;
    verifier.verify_file(&binary)?;
    atomic_copy(&binary, &out.join(&name))?;
    write_receipt(
        &out.join(format!("{name}.handoff.toml")),
        &aterm_toml::to_string(&verifier).map_err(|e| e.to_string())?,
    )?;
    Ok(out.join(name))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix("aterm-linux-handoff-")
            .tempdir()
            .unwrap()
    }

    fn fixture(dir: &Path, target: LinuxTarget) -> Artifact {
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

    /// A synthetic static verifier for `target` beside its receipt: an ELF header and one
    /// `PT_LOAD` program header (`interpreter` makes it `PT_INTERP`, a dynamic one).
    fn verifier_fixture(dir: &Path, target: LinuxTarget, interpreter: bool) -> VerifierArtifact {
        let mut bytes = vec![0u8; 64 + 56];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        let machine: u16 = if target == LinuxTarget::Aarch64 {
            183
        } else {
            62
        };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        let kind = if interpreter { PT_INTERP } else { 1 };
        bytes[64..68].copy_from_slice(&kind.to_le_bytes());
        let asset = pin::asset_name("0.100.0", target.arch());
        fs::write(dir.join(&asset), &bytes).unwrap();
        let verifier = VerifierArtifact {
            schema: 1,
            version: "0.100.0".into(),
            build_number: 100,
            commit: "a".repeat(40),
            target: target.triple().into(),
            asset: asset.clone(),
            sha256: sha256_file(&dir.join(&asset)).unwrap(),
            size: bytes.len() as u64,
            source_fingerprint: "b".repeat(64),
            compiler_version: format!(
                "rustc 1.99.1\nbinary: rustc\nhost: {}\nrelease: 1.99.1\n",
                target.triple()
            ),
        };
        fs::write(
            dir.join(format!("{asset}.handoff.toml")),
            aterm_toml::to_string(&verifier).unwrap(),
        )
        .unwrap();
        verifier
    }

    /// `ship pin-verifier` writes install.sh's four lines and the committed record from a
    /// cut's receipts, and the pair passes `pin::check`; a record this checkout's ledger
    /// does not claim, or of another interface, writes nothing.
    #[test]
    fn pin_verifier_writes_the_pin_and_its_record_together() {
        let root = scratch();
        let script = format!(
            "#!/bin/bash\n# the installer\n{}rest\n",
            Pin::default().lines()
        );
        fs::create_dir_all(root.path().join("tools")).unwrap();
        fs::create_dir_all(root.path().join("crates/aterm-bootstrap-verify")).unwrap();
        fs::write(root.path().join("tools/install.sh"), &script).unwrap();
        fs::write(
            root.path().join("RELEASES.ledger"),
            "# ledger\n100 0.100.0\n",
        )
        .unwrap();
        let record = pin::Record {
            repo: "alabsystems/aterm".into(),
            tag: "v0.100.0".into(),
            build_number: 100,
            commit: "a".repeat(40),
            interface: pin::INTERFACE,
            verifiers: vec![pin::Receipt {
                arch: "x86_64".into(),
                asset: pin::asset_name("0.100.0", "x86_64"),
                sha256: "c".repeat(64),
                size: 4096,
            }],
        };
        let dist = scratch();
        let receipts = dist.path().join(PIN_RECORD);
        let refused = |record: &pin::Record, why: &str| {
            fs::write(&receipts, record.to_toml()).unwrap();
            let said = pin_verifier(root.path(), &receipts).expect_err(why);
            assert!(said.contains(why), "{why}: {said}");
            assert_eq!(
                fs::read_to_string(root.path().join("tools/install.sh")).unwrap(),
                script,
                "{why}: nothing written"
            );
            assert!(!root.path().join(pin::RECORD).exists(), "{why}");
        };
        refused(
            &pin::Record {
                build_number: 101,
                ..record.clone()
            },
            "records no claim",
        );
        refused(
            &pin::Record {
                interface: pin::INTERFACE + 1,
                ..record.clone()
            },
            "speaks interface",
        );
        fs::write(&receipts, record.to_toml()).unwrap();
        assert_eq!(pin_verifier(root.path(), &receipts), Ok(record.pin()));
        let written = fs::read_to_string(root.path().join("tools/install.sh")).unwrap();
        assert_eq!(
            written,
            format!(
                "#!/bin/bash\n# the installer\n{}rest\n",
                record.pin().lines()
            )
        );
        let committed = fs::read_to_string(root.path().join(pin::RECORD)).unwrap();
        assert_eq!(
            pin::check(&written, Some(&committed), "100 0.100.0\n"),
            Ok(record.pin())
        );
    }

    /// A worker's static verifier joins the cut beside its executable: imported into the
    /// stage, uploaded with the Linux assets, its receipt signed in the appcast, and printed as
    /// install.sh's pin, which reads back as it was printed. An architecture with none
    /// stays on install.sh's embedded verifier (its digest empty in the pin).
    #[test]
    fn a_workers_static_verifier_is_staged_and_pinned() {
        let source = scratch();
        let dist = scratch();
        for t in TARGETS {
            fixture(source.path(), t);
        }
        let verifier = verifier_fixture(source.path(), LinuxTarget::Aarch64, false);
        let mut handoff = Handoff::new(source.path(), &[], &[]).unwrap();
        handoff
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        assert_eq!(handoff.verifiers, std::slice::from_ref(&verifier));
        handoff
            .verify_staged(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        verifier
            .verify_file(&dist.path().join(&verifier.asset))
            .unwrap();
        let mut manifest = manifest();
        handoff.stamp(&mut manifest).unwrap();
        assert_eq!(
            manifest_asset_names(&manifest).unwrap(),
            [
                LinuxTarget::X86_64.asset_name("0.100.0"),
                LinuxTarget::Aarch64.asset_name("0.100.0"),
                verifier.asset.clone(),
            ],
            "the appcast signs the verifier beside the executables"
        );
        let commit = "a".repeat(40);
        let record = handoff
            .record("alabsystems/aterm", "0.100.0", 100, &commit)
            .expect("a verifier was frozen");
        assert_eq!(
            signed_verifiers(&manifest).unwrap(),
            Some((record.verifiers.clone(), pin::INTERFACE)),
            "the signed receipts are the record's"
        );
        let text = crate::manifest_out::emit(&manifest).unwrap();
        assert_eq!(
            signed_verifiers(&Manifest::parse(&text).unwrap()).unwrap(),
            Some((record.verifiers.clone(), pin::INTERFACE)),
            "and survive the emitted bytes"
        );
        assert_eq!(record.validate(), Ok(()));
        assert_eq!(pin::Record::parse(&record.to_toml()), Ok(record.clone()));
        assert_eq!(
            record.pin(),
            Pin {
                repo: "alabsystems/aterm".into(),
                tag: "v0.100.0".into(),
                x86_64: String::new(),
                aarch64: verifier.sha256.clone(),
            }
        );
        assert_eq!(
            record.verifiers,
            [pin::Receipt {
                arch: "aarch64".into(),
                asset: verifier.asset.clone(),
                sha256: verifier.sha256.clone(),
                size: verifier.size,
            }]
        );
        assert!(
            Handoff::new(source.path(), &[], &[])
                .unwrap()
                .record("alabsystems/aterm", "0.100.0", 100, &commit)
                .is_none()
        );
        let frozen: Handoff =
            aterm_toml::from_str(&aterm_toml::to_string(&handoff).unwrap()).unwrap();
        assert_eq!(frozen, handoff);
        let old: Handoff = aterm_toml::from_str(
            aterm_toml::to_string(&handoff)
                .unwrap()
                .split("[[verifiers]]")
                .next()
                .unwrap(),
        )
        .unwrap();
        assert!(
            old.verifiers.is_empty(),
            "a journal from before verifiers reads as none"
        );
        let mut published = crate::channel::required_asset_names("0.100.0", false, false);
        let linux: Vec<String> = handoff
            .artifacts
            .iter()
            .map(|a| a.asset.clone())
            .chain([verifier.asset.clone()])
            .collect();
        published.extend(linux.iter().cloned());
        crate::channel::validate_channel_asset_set(&published, "0.100.0", false, false, &linux)
            .unwrap();
        assert!(crate::channel::publishable_asset_names("0.100.0").contains(&verifier.asset));
        let foreign = vec![pin::asset_name("0.99.0", "aarch64")];
        assert!(
            crate::channel::validate_channel_asset_set(
                &published, "0.100.0", false, false, &foreign
            )
            .unwrap_err()
            .to_string()
            .contains("noncanonical")
        );
    }

    /// Negative controls: a dynamic, tampered, misnamed or mislabelled verifier is refused,
    /// and a resumed cut refuses a verifier that appeared, changed or went after it froze.
    #[test]
    fn a_verifier_that_is_not_the_receipts_static_build_is_refused() {
        let source = scratch();
        let dist = scratch();
        let dynamic = verifier_fixture(source.path(), LinuxTarget::Aarch64, true);
        assert!(
            dynamic
                .verify_file(&source.path().join(&dynamic.asset))
                .unwrap_err()
                .contains("static")
        );
        let verifier = verifier_fixture(source.path(), LinuxTarget::Aarch64, false);
        let mut misnamed = verifier.clone();
        misnamed.asset = pin::asset_name("0.100.0", "x86_64");
        assert!(misnamed.validate("0.100.0", 100, &"a".repeat(40)).is_err());
        let mut unknown = verifier.clone();
        unknown.compiler_version = "rustc 1.99.1\n".into();
        assert!(unknown.validate("0.100.0", 100, &"a".repeat(40)).is_err());
        let mut oversized = verifier.clone();
        oversized.size = pin::MAX_ASSET + 1;
        assert!(oversized.validate("0.100.0", 100, &"a".repeat(40)).is_err());
        let mut foreign = verifier.clone();
        foreign.target = LinuxTarget::X86_64.triple().into();
        assert!(
            foreign
                .verify_file(&source.path().join(&verifier.asset))
                .is_err()
        );
        let mut data = fs::read(source.path().join(&verifier.asset)).unwrap();
        data[100] ^= 1;
        fs::write(source.path().join(&verifier.asset), &data).unwrap();
        assert!(
            verifier
                .verify_file(&source.path().join(&verifier.asset))
                .unwrap_err()
                .contains("hash")
        );
        fixture(source.path(), LinuxTarget::Aarch64);
        let mut handoff =
            Handoff::new(source.path(), &[LinuxTarget::Aarch64.triple().into()], &[]).unwrap();
        assert!(
            handoff
                .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
                .unwrap_err()
                .contains("hash"),
            "a tampered verifier fails the import"
        );
        for file in [
            verifier.asset.clone(),
            format!("{}.handoff.toml", verifier.asset),
        ] {
            fs::remove_file(source.path().join(file)).unwrap();
        }
        handoff
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        assert!(handoff.verifiers.is_empty());
        verifier_fixture(source.path(), LinuxTarget::Aarch64, false);
        assert!(
            handoff
                .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
                .unwrap_err()
                .contains("changed"),
            "a verifier that appeared after the cut froze none"
        );
        let mut frozen = handoff.clone();
        frozen.verifiers = vec![verifier.clone()];
        frozen.verifiers[0].sha256 = "f".repeat(64);
        assert!(
            frozen
                .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
                .unwrap_err()
                .contains("changed")
        );
        frozen.verifiers = vec![verifier.clone(), verifier];
        assert!(
            frozen
                .validate_identity("0.100.0", 100, &"a".repeat(40), true)
                .is_err(),
            "two verifiers for one architecture"
        );
    }

    fn manifest() -> Manifest {
        Manifest::parse(&format!("schema=1\nversion=\"0.100.0\"\nbuild_number=100\ncommit=\"{}\"\ndmg=\"aterm-0.100.0.dmg\"\nsha256=\"{}\"\nteam_id=\"\"\n", "a".repeat(40), "f".repeat(64))).unwrap()
    }

    #[test]
    fn imports_both_architectures_and_binds_signed_manifest_fields() {
        let source = scratch();
        let dist = scratch();
        for t in TARGETS {
            fixture(source.path(), t);
        }
        let mut handoff = Handoff::new(source.path(), &[], &[]).unwrap();
        handoff
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        handoff
            .verify_staged(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        let mut manifest = manifest();
        handoff.stamp(&mut manifest).unwrap();
        assert_eq!(manifest_asset_names(&manifest).unwrap().len(), 2);
        assert_eq!(
            signed_verifiers(&manifest),
            Ok(None),
            "no verifier was frozen"
        );
        verify_manifest_handoff(&manifest, Some(&handoff)).unwrap();
        assert!(verify_manifest_handoff(&manifest, None).is_err());
        manifest.linux_aarch64_sha256 = Some("f".repeat(64));
        assert!(verify_manifest_handoff(&manifest, Some(&handoff)).is_err());
    }

    /// The signed verifier fields are held to the frozen handoff like the executables'
    /// (negative control: a digest, a size or an interface the handoff did not freeze, or a
    /// receipt it has none for), and a manifest's receipts are read whole or refused.
    #[test]
    fn signed_verifier_fields_are_bound_to_the_handoff_and_read_whole() {
        let source = scratch();
        let dist = scratch();
        for t in TARGETS {
            fixture(source.path(), t);
            verifier_fixture(source.path(), t, false);
        }
        let mut handoff = Handoff::new(source.path(), &[], &[]).unwrap();
        handoff
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        assert_eq!(handoff.verifiers.len(), 2);
        let mut signed = manifest();
        handoff.stamp(&mut signed).unwrap();
        verify_manifest_handoff(&signed, Some(&handoff)).unwrap();
        let (receipts, interface) = signed_verifiers(&signed).unwrap().unwrap();
        assert_eq!(interface, pin::INTERFACE);
        assert_eq!(
            receipts.iter().map(|r| r.arch.as_str()).collect::<Vec<_>>(),
            pin::ARCHES
        );
        let bent = |f: &dyn Fn(&mut Manifest)| {
            let mut m = signed.clone();
            f(&mut m);
            m
        };
        for (why, m) in [
            (
                "digest",
                bent(&|m| m.linux_verifier_x86_64_sha256 = Some("f".repeat(64))),
            ),
            (
                "size",
                bent(&|m| m.linux_verifier_aarch64_size = Some(4096)),
            ),
            (
                "interface",
                bent(&|m| m.linux_verifier_interface = Some(pin::INTERFACE + 1)),
            ),
        ] {
            assert!(
                verify_manifest_handoff(&m, Some(&handoff)).is_err(),
                "{why}"
            );
        }
        let mut bare = Handoff::new(source.path(), &[], &[]).unwrap();
        bare.import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        bare.verifiers.clear();
        assert!(
            verify_manifest_handoff(&signed, Some(&bare)).is_err(),
            "a signed receipt the handoff never froze"
        );
        for (why, m) in [
            ("incomplete", bent(&|m| m.linux_verifier_x86_64_size = None)),
            (
                "canonical",
                bent(&|m| m.linux_verifier_x86_64 = Some(pin::asset_name("0.99.0", "x86_64"))),
            ),
            (
                "lowercase",
                bent(&|m| m.linux_verifier_aarch64_sha256 = Some("F".repeat(64))),
            ),
            (
                "outside",
                bent(&|m| m.linux_verifier_aarch64_size = Some(63)),
            ),
            (
                "outside",
                bent(&|m| m.linux_verifier_aarch64_size = Some(pin::MAX_ASSET + 1)),
            ),
            ("no interface", bent(&|m| m.linux_verifier_interface = None)),
            ("no verifier", {
                let mut m = manifest();
                m.linux_verifier_interface = Some(pin::INTERFACE);
                m
            }),
        ] {
            let said = signed_verifiers(&m).expect_err(why);
            assert!(said.contains(why), "{why}: {said}");
        }
    }

    #[test]
    fn explicit_native_subset_is_honest_and_default_refuses_missing_worker() {
        let source = scratch();
        let dist = scratch();
        fixture(source.path(), LinuxTarget::Aarch64);
        assert!(
            Handoff::new(source.path(), &[], &[])
                .unwrap()
                .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
                .is_err()
        );
        let mut handoff =
            Handoff::new(source.path(), &[LinuxTarget::Aarch64.triple().into()], &[]).unwrap();
        handoff
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        let mut manifest = manifest();
        handoff.stamp(&mut manifest).unwrap();
        assert!(manifest.linux_x86_64.is_none());
        assert!(manifest.linux_aarch64.is_some());
        assert!(Handoff::new(source.path(), &["riscv64".into()], &[]).is_err());
        assert!(
            Handoff::new(
                source.path(),
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
        let source = scratch();
        let dist = scratch();
        let mut artifact = fixture(source.path(), LinuxTarget::Aarch64);
        let mut handoff =
            Handoff::new(source.path(), std::slice::from_ref(&artifact.target), &[]).unwrap();
        handoff
            .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        let frozen: Handoff =
            aterm_toml::from_str(&aterm_toml::to_string(&handoff).unwrap()).unwrap();
        assert_eq!(frozen, handoff);
        artifact.compiler_sha256 = "f".repeat(64);
        fs::write(
            source
                .path()
                .join(format!("{}.handoff.toml", artifact.asset)),
            aterm_toml::to_string(&artifact).unwrap(),
        )
        .unwrap();
        assert!(
            handoff
                .import(dist.path(), "0.100.0", 100, &"a".repeat(40))
                .unwrap_err()
                .contains("changed")
        );
        let mut data = fs::read(dist.path().join(&artifact.asset)).unwrap();
        data[70] ^= 1;
        fs::write(dist.path().join(&artifact.asset), data).unwrap();
        assert!(
            frozen
                .verify_staged(dist.path(), "0.100.0", 100, &"a".repeat(40))
                .unwrap_err()
                .contains("hash")
        );
    }

    #[test]
    fn wrong_build_commit_arch_and_symlink_are_refused() {
        let source = scratch();
        let dist = scratch();
        let mut artifact = fixture(source.path(), LinuxTarget::Aarch64);
        assert!(artifact.validate("0.100.0", 101, &"a".repeat(40)).is_err());
        assert!(artifact.validate("0.100.0", 100, &"b".repeat(40)).is_err());
        artifact.target = LinuxTarget::X86_64.triple().into();
        assert!(
            artifact
                .verify_file(&source.path().join(&artifact.asset))
                .is_err()
        );
        let alias = dist.path().join("alias");
        std::os::unix::fs::symlink(source.path().join(&artifact.asset), &alias).unwrap();
        assert!(artifact.verify_file(&alias).is_err());
        assert!(check_identity("0.100.0", 0, &"a".repeat(40)).is_err());
        assert!(check_identity("0.100.0", 1, "short").is_err());
    }

    #[test]
    fn the_channel_release_requires_every_signed_raw_asset_exactly_once() {
        let source = scratch();
        let artifact = fixture(source.path(), LinuxTarget::Aarch64);
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
    fn a_real_cut_declares_its_linux_shape() {
        let parse = |args: &[&str]| {
            crate::cli::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(parse(&["cut"]).unwrap_err(), crate::cli::MAC_ONLY_REFUSAL);
        assert!(parse(&["cut", "--mac-only"]).is_ok());
        assert!(parse(&["cut", "--linux-artifacts", "/private/output"]).is_ok());
        assert!(parse(&["cut", "--dry-run"]).is_ok());
        assert!(parse(&["cut", "--resume"]).is_ok());
        for refused in [
            &["cut", "--mac-only", "--linux-artifacts", "/private/output"][..],
            &["cut", "--resume", "--mac-only"],
            &["cut", "--resume", "--linux-artifacts", "/changed/output"],
            &["cut", "--linux-target", "aarch64"],
            &["cut", "--mac-only", "--linux-worker", "aarch64=h:/r"],
        ] {
            assert!(parse(refused).is_err(), "{refused:?}");
        }
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
        let dir = scratch();
        let aarch64 = LinuxTarget::Aarch64.triple().to_string();
        let handoff = Handoff::new(
            dir.path(),
            std::slice::from_ref(&aarch64),
            &["aarch64=h:/r".into()],
        )
        .unwrap();
        assert_eq!(handoff.workers.len(), 1);
        assert!(
            Handoff::new(
                dir.path(),
                std::slice::from_ref(&aarch64),
                &["x86_64=h:/r".into()]
            )
            .is_err(),
            "a worker for an architecture the cut does not declare"
        );
        assert!(
            Handoff::new(
                dir.path(),
                &[],
                &["aarch64=a:/r".into(), "aarch64=b:/r".into()]
            )
            .is_err(),
            "two workers for one architecture"
        );
    }

    #[test]
    fn only_this_cuts_valid_pair_counts_as_already_built() {
        let dir = scratch();
        let target = LinuxTarget::Aarch64;
        let commit = "a".repeat(40);
        assert!(
            !pair_is_current(dir.path(), target, "0.100.0", 100, &commit),
            "nothing there"
        );
        fixture(dir.path(), target);
        assert!(pair_is_current(dir.path(), target, "0.100.0", 100, &commit));
        assert!(!pair_is_current(
            dir.path(),
            target,
            "0.100.0",
            101,
            &commit
        ));
        assert!(!pair_is_current(
            dir.path(),
            target,
            "0.100.0",
            100,
            &"b".repeat(40)
        ));
        assert!(!pair_is_current(
            dir.path(),
            LinuxTarget::X86_64,
            "0.100.0",
            100,
            &commit
        ));
        let asset = dir.path().join(target.asset_name("0.100.0"));
        let mut bytes = fs::read(&asset).unwrap();
        bytes.truncate(70);
        fs::write(&asset, bytes).unwrap();
        assert!(!pair_is_current(
            dir.path(),
            target,
            "0.100.0",
            100,
            &commit
        ));
    }

    #[test]
    fn the_worker_script_is_parsed_whole_before_it_runs() {
        assert!(WORKER_SCRIPT.starts_with("main() {\n"));
        assert!(WORKER_SCRIPT.trim_end().ends_with("main \"$@\" </dev/null"));
        assert!(WORKER_SCRIPT.contains("umask 077"));
        assert!(WORKER_SCRIPT.contains("mktemp -d"));
        if std::path::Path::new("/bin/bash").exists() {
            let status = command("/bin/bash")
                .args(["-n", "-c", WORKER_SCRIPT])
                .status()
                .unwrap();
            assert!(status.success(), "bash -n rejects the worker script");
        }
    }
}
