// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows handoff a cut admits (`--windows-artifacts DIRECTORY`), as it admits a native
//! Linux one (linux_release.rs, [`super::linux::Handoff`]): the directory is fixed before the
//! claim, the pair in it is held to the cut when it signs, frozen into the journal, copied
//! into `dist/`, stamped into the signed appcast and uploaded with the release. Unlike a
//! Linux pair, which a `--linux-worker` can fetch itself, a Windows pair always comes by hand,
//! so it is also held to the zip's SHA-256 the person read off the Windows host and named
//! (`--windows-sha256`): the pair's own receipt binds nothing a writer of the carry cannot set.
//!
//! The pair is what `aterm-winship release-build` writes on a Windows host (the Mac
//! cannot build the MSVC images, and this crate does not build on Windows): the portable
//! zip `aterm-<version>-windows-x86_64.zip` and its receipt. What a receipt is, and what
//! admits a directory, is aterm-winship's one definition (`aterm_winship::handoff`), so the
//! writer and this reader agree by compilation; this module adds what only the cutter knows:
//! the commit's tree and pins read from its own repository ([`Sources`]), the private
//! directory, the staged copy and the appcast's fields.
//!
//! The zip also carries get-aterm.ps1's signature checker (`handoff::VERIFIER_MEMBER`). The
//! cut takes it out of the admitted zip, publishes it beside the zip as
//! `aterm-bootstrap-verify-<version>-windows-x86_64.exe` and signs its digest into the
//! appcast's `windows_verifier_*` fields (`crate::windows_pin::stamp`), which
//! `ship pin-verifier --windows --from-release` reads back. Inside the zip it is bound by the
//! digest the person named, as the images are.

use std::path::{Path, PathBuf};

use aterm_bootstrap_verify::pin;
use aterm_update_core::Manifest;
use aterm_update_core::windows::WindowsArtifact;
use aterm_winship::handoff::{self, Cut, Pins, Receipt, TARGET};
use serde::{Deserialize, Serialize};

use crate::ledger::{GitRunner, git_ok};

/// The directory is fixed before the claim; its pair is frozen when the cut signs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub directory: PathBuf,
    /// The zip's SHA-256 the person named (`--windows-sha256`), read off the Windows host
    /// where `release-build` printed it: what binds a pair that came by hand
    /// ([`Receipt::check_carried`]). The sign step admits no pair without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The receipt the sign step admitted, frozen: a resume refuses a directory that says
    /// anything else.
    #[serde(default)]
    pub artifact: Option<Receipt>,
    /// The verifier the sign step took out of the frozen zip, frozen with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier: Option<Verifier>,
}

/// get-aterm.ps1's verifier as the cut took it out of the frozen zip: the digest and size
/// the appcast signs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verifier {
    /// Lowercase SHA-256 of the executable.
    pub sha256: String,
    /// Its size in bytes.
    pub size: u64,
}

impl Verifier {
    /// The verifier as the appcast of `version` signs it.
    fn receipt(&self, version: &str) -> pin::Receipt {
        pin::Receipt {
            arch: TARGET.arch().to_string(),
            asset: verifier_asset(version),
            sha256: self.sha256.clone(),
            size: self.size,
        }
    }
}

/// The verifier's asset name for `version`.
fn verifier_asset(version: &str) -> String {
    pin::windows_asset_name(version, TARGET.arch())
}

/// The verifier the staged zip at `zip` carries, written beside it in `dist` under its asset
/// name for `version` and read back from there.
fn stage_verifier(zip: &Path, dist: &Path, version: &str) -> Result<Verifier, String> {
    let file = std::fs::File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let bytes = handoff::zip_verifier(file).map_err(|why| format!("{}: {why}", zip.display()))?;
    pin::windows_verifier_image(&bytes).map_err(|why| format!("{}: {why}", zip.display()))?;
    let verifier = Verifier {
        sha256: crate::publish::sha256_bytes(&bytes),
        size: u64::try_from(bytes.len()).map_err(|e| e.to_string())?,
    };
    let at = dist.join(verifier_asset(version));
    crate::durable::write_atomic_durable(&at, &bytes, 0o644)
        .map_err(|e| format!("stage {}: {e}", at.display()))?;
    crate::windows_pin::windows_verifier_file(&at, verifier.size, &verifier.sha256)?;
    Ok(verifier)
}

/// What the release commit is and pins, read from this repository: its root tree and the
/// compilers a Windows build of it may use ([`handoff::Pins`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources {
    /// `git rev-parse <commit>^{tree}`.
    pub tree: String,
    /// The stock release the commit's `stock_pin.rs` pins.
    pub stock: String,
    /// The commit's `trust.lock` `compiler_commit`, when it carries a lock.
    pub trust_commit: Option<String>,
}

impl Sources {
    /// The commit's tree and pins.
    ///
    /// # Errors
    /// What git cannot answer for `commit`, or a pin that does not read.
    pub fn at(git: &dyn GitRunner, commit: &str) -> Result<Self, String> {
        let read = |args: &[&str]| {
            git_ok(git, args)
                .map(|out| out.stdout_utf8())
                .map_err(|e| e.to_string())
        };
        let tree = read(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])?
            .trim()
            .to_string();
        let source = read(&["show", &format!("{commit}:{}", handoff::STOCK_PIN_FILE)])?;
        let stock = handoff::stock_pin(&source)
            .ok_or_else(|| format!("{commit}:{} pins no STOCK_PIN", handoff::STOCK_PIN_FILE))?
            .to_string();
        let trust_commit =
            crate::trust_lock::lock_at(git, commit)?.map(|lock| lock.compiler_commit);
        Ok(Self {
            tree,
            stock,
            trust_commit,
        })
    }

    /// The pins as admission reads them.
    #[must_use]
    pub fn pins(&self) -> Pins<'_> {
        Pins {
            stock: &self.stock,
            trust_commit: self.trust_commit.as_deref(),
        }
    }
}

/// The Windows assets `manifest` signs: the zip, then the verifier.
///
/// # Errors
/// Windows fields the manifest's readers refuse.
pub fn manifest_asset_names(manifest: &Manifest) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = manifest
        .windows_artifact(TARGET)?
        .map(|artifact| vec![artifact.name.to_string()])
        .unwrap_or_default();
    if let Some((receipts, _)) = crate::windows_pin::signed_windows_verifiers(manifest)? {
        names.extend(receipts.into_iter().map(|receipt| receipt.asset));
    }
    Ok(names)
}

/// The appcast's Windows fields, the zip's and the verifier's, are exactly what `handoff`
/// froze: none without one.
///
/// # Errors
/// Fields that disagree with the frozen handoff, or that the manifest's readers refuse.
pub fn verify_manifest_handoff(
    manifest: &Manifest,
    handoff: Option<&Handoff>,
) -> Result<(), String> {
    let mut expected = manifest.clone();
    expected.windows_x86_64 = None;
    expected.windows_x86_64_sha256 = None;
    expected.windows_x86_64_size = None;
    expected.windows_verifier_x86_64 = None;
    expected.windows_verifier_x86_64_sha256 = None;
    expected.windows_verifier_x86_64_size = None;
    expected.windows_verifier_interface = None;
    if let Some(handoff) = handoff {
        handoff.stamp(&mut expected)?;
    }
    let fields = |m: &Manifest| -> Result<_, String> {
        Ok((
            m.windows_artifact(TARGET)?
                .map(|a| (a.name.to_string(), a.sha256.to_string(), a.size)),
            crate::windows_pin::signed_windows_verifiers(m)?,
        ))
    };
    if fields(manifest)? != fields(&expected)? {
        return Err("signed Windows fields disagree with the cut's frozen Windows handoff".into());
    }
    Ok(())
}

fn check_identity(version: &str, build: u64, commit: &str) -> Result<(), String> {
    crate::ledger::check_version_shape(version).map_err(|e| e.to_string())?;
    if build == 0 || !aterm_codec::hex::is_lower(commit, 40) {
        return Err(
            "a Windows handoff is of a positive claimed build and a full lowercase source commit"
                .into(),
        );
    }
    Ok(())
}

impl Handoff {
    /// The handoff `--windows-artifacts directory` names: canonical, and a private directory
    /// of this user's, as a Linux handoff's; bound to `sha256`, the `--windows-sha256` the
    /// person named, when they named one.
    ///
    /// # Errors
    /// A directory that does not resolve, or is not private; a digest that is no SHA-256.
    pub fn new(directory: &Path, sha256: Option<&str>) -> Result<Self, String> {
        let directory = directory
            .canonicalize()
            .map_err(|e| format!("Windows handoff directory: {e}"))?;
        super::linux::private_handoff_directory(&directory, "Windows handoff")?;
        Ok(Self {
            directory,
            sha256: sha256.map(handoff::carried_sha256).transpose()?,
            artifact: None,
            verifier: None,
        })
    }

    /// The handoff is of release `version` build `build` at `commit`, from a safe directory;
    /// `complete`: its zip is frozen.
    ///
    /// # Errors
    /// Which of those it is not.
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
        {
            return Err("Windows handoff has an unsafe directory".into());
        }
        if let Some(sha256) = &self.sha256
            && !aterm_codec::hex::is_lower(sha256, 64)
        {
            return Err("Windows handoff names a carried digest that is no SHA-256".into());
        }
        match (&self.artifact, &self.verifier) {
            (Some(artifact), Some(verifier)) => {
                artifact.check_identity(version, build, commit)?;
                if !aterm_codec::hex::is_lower(&verifier.sha256, 64) {
                    return Err("Windows handoff froze a verifier digest that is no SHA-256".into());
                }
                // A frozen zip was bound to the digest the person named when it was admitted.
                artifact.check_carried(self.sha256.as_deref())
            }
            (Some(_), None) => {
                Err("Windows handoff froze its zip but not the verifier it carries".into())
            }
            (None, Some(_)) => Err("Windows handoff froze a verifier but no zip".into()),
            (None, None) if complete => Err("Windows handoff has not frozen its zip".into()),
            (None, None) => Ok(()),
        }
    }

    /// Admit the directory's pair for this cut ([`handoff::admit`] under the commit's
    /// [`Sources`]) and copy its zip into `dist`. A handoff that froze a receipt admits only
    /// that receipt again.
    ///
    /// # Errors
    /// A pair that is missing (the error says how to make it), not admitted, or not the one
    /// frozen; a stage that cannot be written.
    pub fn import(
        &mut self,
        dist: &Path,
        version: &str,
        build: u64,
        commit: &str,
        sources: &Sources,
    ) -> Result<(), String> {
        self.validate_identity(version, build, commit, false)?;
        super::linux::private_handoff_directory(&self.directory, "Windows handoff")?;
        match std::fs::create_dir(dist) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create Windows release stage: {e}")),
        }
        drop(super::open_release_directory(
            dist,
            super::current_release_uid()?,
            true,
            "Windows release stage",
        )?);
        let lacking = handoff::missing(&self.directory, version)?;
        if !lacking.is_empty() {
            return Err(format!(
                "Windows build output missing ({}) in {}. On a Windows host, in a clean \
                 checkout at commit {commit} (before the claim it is on no remote: carry it \
                 with `git bundle create <file> {reserved} ^{commit}^1` here and `git fetch \
                 <file> {reserved}` there), run: aterm-winship release-build --version \
                 {version} --build-number {build} --commit {commit} --out <empty-directory>; \
                 copy its two files into {} and run the release again with --windows-sha256 \
                 <the zip's SHA-256 its last line printed, read on the Windows host>",
                lacking.join(" and "),
                self.directory.display(),
                self.directory.display(),
                reserved = crate::ledger::RESERVED_REF,
            ));
        }
        let cut = Cut {
            version,
            build,
            commit,
            source_tree: &sources.tree,
        };
        let admitted = handoff::admit(&self.directory, &cut, &sources.pins())?;
        if self
            .artifact
            .as_ref()
            .is_some_and(|frozen| *frozen != admitted)
        {
            return Err(
                "Windows build inputs changed after this cut froze their identity; resume refused"
                    .into(),
            );
        }
        // Admission proves the pair agrees with itself and this cut; the digest the person read
        // off the Windows host proves it is the pair that host built.
        admitted.check_carried(self.sha256.as_deref())?;
        let staged = dist.join(&admitted.asset);
        super::linux::stage_copy(
            &self.directory.join(&admitted.asset),
            &staged,
            0o644,
            "Windows zip",
        )?;
        admitted.verify_zip(&staged)?;
        let verifier = stage_verifier(&staged, dist, version)?;
        if self
            .verifier
            .as_ref()
            .is_some_and(|frozen| *frozen != verifier)
        {
            return Err(
                "the Windows zip's verifier changed after this cut froze its identity; resume \
                 refused"
                    .into(),
            );
        }
        self.artifact = Some(admitted);
        self.verifier = Some(verifier);
        Ok(())
    }

    /// The frozen zip and its verifier are staged in `dist` as frozen, and the verifier is the
    /// one the staged zip carries.
    ///
    /// # Errors
    /// No frozen zip, another cut's, or staged bytes that are not its.
    pub fn verify_staged(
        &self,
        dist: &Path,
        version: &str,
        build: u64,
        commit: &str,
    ) -> Result<(), String> {
        self.validate_identity(version, build, commit, true)?;
        let (Some(artifact), Some(verifier)) = (&self.artifact, &self.verifier) else {
            return Err("Windows handoff has not frozen its zip".into());
        };
        let zip = dist.join(&artifact.asset);
        artifact.verify_zip(&zip)?;
        let file = std::fs::File::open(&zip).map_err(|e| format!("{}: {e}", zip.display()))?;
        let carried = handoff::zip_verifier(file)?;
        if crate::publish::sha256_bytes(&carried) != verifier.sha256 {
            return Err(format!(
                "the frozen Windows verifier is not the one {} carries",
                zip.display()
            ));
        }
        crate::windows_pin::windows_verifier_file(
            &dist.join(verifier_asset(version)),
            verifier.size,
            &verifier.sha256,
        )
    }

    /// Write the frozen zip's name, digest and size into `manifest`'s Windows fields, and its
    /// verifier's into the `windows_verifier_*` fields (`crate::windows_pin::stamp`).
    ///
    /// # Errors
    /// A receipt of another release than `manifest`'s, or fields its readers refuse:
    /// `manifest` is left as it was.
    pub fn stamp(&self, manifest: &mut Manifest) -> Result<(), String> {
        let Some(artifact) = &self.artifact else {
            return Ok(());
        };
        let verifier = self
            .verifier
            .as_ref()
            .ok_or("Windows handoff froze its zip but not the verifier it carries")?;
        artifact.check_identity(
            &manifest.version,
            manifest.build_number,
            manifest
                .commit
                .as_deref()
                .ok_or("Windows manifest requires a source commit")?,
        )?;
        let mut stamped = manifest.clone();
        stamped.set_windows_artifact(&WindowsArtifact {
            target: TARGET,
            name: &artifact.asset,
            sha256: &artifact.sha256,
            size: artifact.size,
        })?;
        let receipt = verifier.receipt(&stamped.version);
        crate::windows_pin::stamp(&mut stamped, &receipt)?;
        *manifest = stamped;
        Ok(())
    }

    /// The assets the cut publishes: the frozen zip and its verifier, or none before the sign
    /// step.
    #[must_use]
    pub fn asset_names(&self) -> Vec<String> {
        self.artifact
            .iter()
            .flat_map(|artifact| [artifact.asset.clone(), verifier_asset(&artifact.version)])
            .collect()
    }
}

#[cfg(all(test, unix))]
#[path = "windows_release_tests.rs"]
mod tests;
