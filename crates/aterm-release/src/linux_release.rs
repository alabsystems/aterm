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
    pub fn new(directory: &Path, targets: &[String]) -> Result<Self, String> {
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
        Ok(Self {
            directory,
            targets,
            artifacts: Vec::new(),
        })
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
                 --commit {commit} --out <handoff-directory>; copy its pair into {} and resume the cut",
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
        let mut handoff = Handoff::new(&source.0, &[]).unwrap();
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
            Handoff::new(&source.0, &[])
                .unwrap()
                .import(&dist.0, "0.100.0", 100, &"a".repeat(40))
                .is_err()
        );
        let mut handoff = Handoff::new(&source.0, &[LinuxTarget::Aarch64.triple().into()]).unwrap();
        handoff
            .import(&dist.0, "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        let mut manifest = manifest();
        handoff.stamp(&mut manifest).unwrap();
        assert!(manifest.linux_x86_64.is_none());
        assert!(manifest.linux_aarch64.is_some());
        assert!(Handoff::new(&source.0, &["riscv64".into()]).is_err());
        assert!(
            Handoff::new(
                &source.0,
                &[
                    LinuxTarget::Aarch64.triple().into(),
                    LinuxTarget::Aarch64.triple().into()
                ]
            )
            .is_err()
        );
    }

    #[test]
    fn resume_refuses_changed_provenance_and_tampered_staged_bytes() {
        let source = Scratch::new();
        let dist = Scratch::new();
        let mut artifact = fixture(&source.0, LinuxTarget::Aarch64);
        let mut handoff = Handoff::new(&source.0, std::slice::from_ref(&artifact.target)).unwrap();
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
    fn draft_and_mirror_require_every_signed_raw_asset_exactly_once() {
        let source = Scratch::new();
        let dist = Scratch::new();
        let artifact = fixture(&source.0, LinuxTarget::Aarch64);
        let mut handoff = Handoff::new(&source.0, std::slice::from_ref(&artifact.target)).unwrap();
        handoff
            .import(&dist.0, "0.100.0", 100, &"a".repeat(40))
            .unwrap();
        let mut manifest = manifest();
        handoff.stamp(&mut manifest).unwrap();
        let mut names: Vec<String> = [
            "aterm-appcast.toml",
            "aterm-0.100.0.dmg",
            "aterm-0.100.0.dmg.sha256",
            "aterm-0.100.0-build.txt",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        assert!(
            crate::publish::validate_draft_asset_set(
                &names,
                &manifest,
                false,
                "aterm-0.100.0-build.txt",
                None
            )
            .is_err()
        );
        names.push(artifact.asset.clone());
        crate::publish::validate_draft_asset_set(
            &names,
            &manifest,
            false,
            "aterm-0.100.0-build.txt",
            None,
        )
        .unwrap();
        names.push(artifact.asset.clone());
        assert!(
            crate::publish::validate_draft_asset_set(
                &names,
                &manifest,
                false,
                "aterm-0.100.0-build.txt",
                None
            )
            .is_err()
        );
        let mut mirrored = crate::mirror::required_asset_names("0.100.0", false, false);
        assert!(
            crate::mirror::validate_mirror_asset_set_with_linux(
                &mirrored,
                "0.100.0",
                false,
                false,
                std::slice::from_ref(&artifact.asset)
            )
            .is_err()
        );
        mirrored.push(artifact.asset.clone());
        crate::mirror::validate_mirror_asset_set_with_linux(
            &mirrored,
            "0.100.0",
            false,
            false,
            &[artifact.asset],
        )
        .unwrap();
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
}
