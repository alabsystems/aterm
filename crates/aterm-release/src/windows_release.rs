// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows handoff a cut admits (`--windows-artifacts DIRECTORY`), as it admits a native
//! Linux one (linux_release.rs, [`super::linux::Handoff`]): the directory is fixed before the
//! claim, the pair in it is held to the cut when it signs, frozen into the journal, copied
//! into `dist/`, stamped into the signed appcast and uploaded with the release. Unlike a
//! Linux pair, which a `--linux-worker` can fetch itself, a Windows pair has no worker the cut
//! drives, so it is also held to an anchor the pair's own receipt cannot set: the zip's SHA-256
//! the person read off the Windows host and named (`--windows-sha256`), for a pair that came by
//! hand; or, for one the pull lane brought (`--windows-attested`,
//! `crate::buildplan::windows_pull`), the attestation beside the receipt, by a builder the
//! release commit itself pins (`aterm_winship::attest`, `apps/aterm-win/release-builders.toml`).
//! A handoff has exactly one anchor.
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
    /// ([`Receipt::check_carried`]). The sign step admits no pair without it or `attested`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// `--windows-attested`: the pair came through the pull lane, and what binds it is the
    /// attestation beside its receipt (`<zip>.handoff.sig`) by a builder the release commit pins,
    /// in place of a typed digest ([`Sources::builders`]). Never with `sha256`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub attested: bool,
    /// The receipt the sign step admitted, frozen: a resume refuses a directory that says
    /// anything else.
    #[serde(default)]
    pub artifact: Option<Receipt>,
    /// The verifier the sign step took out of the frozen zip, frozen with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier: Option<Verifier>,
    /// For an `attested` handoff, the builder whose attestation admitted the frozen zip, frozen
    /// with it: a resume refuses an attestation of another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attester: Option<Attester>,
    /// For an `attested` handoff, the UTC day (`YYYY-MM-DD`) its attestation was first admitted
    /// on: the pull lane's admission for a pair it brought (the release journal's
    /// `windows_pull.admitted_on`), else the first import. The builder's `not_after` is judged
    /// on this day at every import, so a resume after it neither refuses what was admitted
    /// before it nor admits what was not. Frozen with the zip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attested_on: Option<String>,
}

/// The builder that attested a pulled pair, as the release commit pins it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attester {
    /// Its id (`aterm_winship::attest::check_id`).
    pub builder: String,
    /// Its pinned Ed25519 public key, 64 lowercase hex digits.
    pub public_key: String,
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
    /// The text of the commit's `apps/aterm-win/release-builders.toml`
    /// (`aterm_winship::pull::BUILDERS_FILE`), the builders whose attestation binds a pulled
    /// pair; `None` when the commit holds no such file, which pins none.
    pub builders: Option<String>,
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
            builders: builders_at(git, commit)?,
        })
    }

    /// The builders the commit pins (none without its builders file), for an attestation
    /// checked on `today` (`YYYY-MM-DD`, UTC): the builder that attested the receipt beside the
    /// zip `asset` in `directory`.
    ///
    /// # Errors
    /// A builders file that does not read, or an attestation `aterm_winship::attest` refuses
    /// (an unknown or expired builder, a bad signature), named as such.
    pub fn attester(&self, directory: &Path, asset: &str, today: &str) -> Result<Attester, String> {
        let builders = match &self.builders {
            Some(text) => {
                aterm_winship::attest::Builders::parse(text).map_err(|e| e.to_string())?
            }
            None => aterm_winship::attest::Builders::default(),
        };
        let attested = aterm_winship::attest::verify_beside(directory, asset, &builders, today)
            .map_err(|refused| format!("the Windows pair's attestation is refused: {refused}"))?;
        Ok(Attester {
            builder: attested.builder,
            public_key: attested.public_key,
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

/// The text of `commit`'s `apps/aterm-win/release-builders.toml` (`aterm_winship::pull::BUILDERS_FILE`),
/// read from this repository's objects; `None` when the commit holds no such file.
///
/// # Errors
/// What git cannot answer for `commit`.
pub fn builders_at(git: &dyn GitRunner, commit: &str) -> Result<Option<String>, String> {
    let read = |args: &[&str]| {
        git_ok(git, args)
            .map(|out| out.stdout_utf8())
            .map_err(|e| e.to_string())
    };
    let builders_file = aterm_winship::pull::BUILDERS_FILE;
    let listed = read(&["ls-tree", "--name-only", commit, "--", builders_file])?;
    if listed.trim().is_empty() {
        return Ok(None);
    }
    read(&["show", &format!("{commit}:{builders_file}")]).map(Some)
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
            attested: false,
            artifact: None,
            verifier: None,
            attester: None,
            attested_on: None,
        })
    }

    /// The handoff a cut declares: `--windows-artifacts directory` with its anchor, the digest
    /// the person named (`--windows-sha256`, [`Handoff::new`]) or, `attested`, the attestation
    /// the pull lane brought beside the receipt (`--windows-attested`).
    ///
    /// # Errors
    /// What [`Handoff::new`] refuses, or both anchors at once.
    pub fn declared(
        directory: &Path,
        sha256: Option<&str>,
        attested: bool,
    ) -> Result<Self, String> {
        if attested && sha256.is_some() {
            return Err(
                "--windows-attested and --windows-sha256 both anchor the Windows pair: a pair \
                 the pull lane brought is bound by its attestation, one carried by hand by the \
                 digest typed; pass one"
                    .into(),
            );
        }
        let mut handoff = Self::new(directory, sha256)?;
        handoff.attested = attested;
        Ok(handoff)
    }

    /// The handoff, for an attested one, judged on `day` (`YYYY-MM-DD`): the day the pull lane
    /// admitted its attestation ([`Handoff::attested_on`]). `None`, or a handoff bound by a
    /// digest, changes nothing.
    ///
    /// # Errors
    /// A day that is no date.
    pub fn attested_on(mut self, day: Option<&str>) -> Result<Self, String> {
        if let Some(day) = day.filter(|_| self.attested) {
            if aterm_winship::pull::date_days(day).is_none() {
                return Err(format!(
                    "the Windows pair's admission day {day:?} is not a date (YYYY-MM-DD)"
                ));
            }
            self.attested_on = Some(day.to_string());
        }
        Ok(self)
    }

    /// The anchor in words, for a refusal: the typed digest, the attestation, or none named.
    #[must_use]
    pub fn anchor_words(&self) -> String {
        match (&self.sha256, self.attested) {
            (Some(sha256), _) => format!("SHA-256 {sha256}"),
            (None, true) => self.attester.as_ref().map_or_else(
                || "attested by a pinned builder".to_string(),
                |attester| format!("attested by builder {}", attester.builder),
            ),
            (None, false) => "no anchor named".to_string(),
        }
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
        if self.attested && self.sha256.is_some() {
            return Err(
                "Windows handoff names both a carried digest and an attestation: one anchor \
                 binds a pair"
                    .into(),
            );
        }
        if !self.attested && self.attester.is_some() {
            return Err("Windows handoff names an attesting builder but no attestation".into());
        }
        if let Some(day) = &self.attested_on
            && (!self.attested || aterm_winship::pull::date_days(day).is_none())
        {
            return Err(
                "Windows handoff names an admission day that is no date, or no attestation it \
                 was admitted by"
                    .into(),
            );
        }
        match (&self.artifact, &self.verifier) {
            (Some(artifact), Some(verifier)) => {
                artifact.check_identity(version, build, commit)?;
                if !aterm_codec::hex::is_lower(&verifier.sha256, 64) {
                    return Err("Windows handoff froze a verifier digest that is no SHA-256".into());
                }
                if !self.attested {
                    // A frozen zip was bound to the digest the person named when it was
                    // admitted.
                    return artifact.check_carried(self.sha256.as_deref());
                }
                // A frozen pulled zip was bound to a pinned builder's attestation when it was
                // admitted: the builder and the day are frozen with it.
                if self.attested_on.is_none() {
                    return Err(
                        "Windows handoff froze an attested zip but not the day it was admitted"
                            .into(),
                    );
                }
                match &self.attester {
                    Some(attester)
                        if aterm_winship::attest::check_id(&attester.builder).is_ok()
                            && aterm_codec::hex::is_lower(&attester.public_key, 64) =>
                    {
                        Ok(())
                    }
                    Some(_) => Err(
                        "Windows handoff froze an attesting builder that is no pinned builder"
                            .into(),
                    ),
                    None => Err(
                        "Windows handoff froze an attested zip but not the builder that attested \
                         it"
                        .into(),
                    ),
                }
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
        if !lacking.is_empty() && self.attested {
            return Err(format!(
                "the pulled Windows pair lacks {} in {}: the release put it there from origin's \
                 handoff before it declared it, and something removed it since; `{} --forget` \
                 withdraws this reservation, and the release reserves and asks again",
                lacking.join(" and "),
                self.directory.display(),
                crate::release::RELEASE_COMMAND,
            ));
        }
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
        // off the Windows host proves it is the pair that host built, or, for a pair the pull
        // lane brought, the attestation of a builder this commit pins proves its receipt is the
        // one that builder signed. Its expiry is judged on the day it was first admitted, never
        // again on a later one: a resume past the builder's `not_after` signs what was admitted
        // before it.
        let attested_on = self.attested.then(|| {
            self.attested_on
                .clone()
                .unwrap_or_else(aterm_winship::pull::today)
        });
        let attester = if let Some(day) = &attested_on {
            let attester = sources.attester(&self.directory, &admitted.asset, day)?;
            if self
                .attester
                .as_ref()
                .is_some_and(|frozen| *frozen != attester)
            {
                return Err(
                    "the Windows pair's attesting builder changed after this cut froze it; \
                     resume refused"
                        .into(),
                );
            }
            Some(attester)
        } else {
            admitted.check_carried(self.sha256.as_deref())?;
            None
        };
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
        self.attester = attester;
        self.attested_on = attested_on;
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
