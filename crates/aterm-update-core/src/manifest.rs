// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The release manifest, the `aterm-appcast.toml` asset on every GitHub release.

use serde::{Deserialize, Serialize};

/// The highest manifest `schema` this build understands.
pub const SUPPORTED_SCHEMA: u32 = 1;

/// The release manifest attached to a GitHub Release as `aterm-appcast.toml`.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format version (the publisher emits `schema = 1`).
    #[serde(default)]
    pub schema: u32,
    /// The release version.
    pub version: String,
    /// Monotonic build number: the downgrade gate.
    pub build_number: u64,
    /// Full git commit hash of the source the release was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The DMG asset's file name within the same release.
    pub dmg: String,
    /// SHA-256 (lowercase hex) of the DMG asset's bytes.
    pub sha256: String,
    /// Absolute browser download URL of the DMG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The ZIP asset's file name, e.g. `"aterm-0.10.0-mac.zip"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    /// SHA-256 (lowercase hex) of [`Self::zip`]'s bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_sha256: Option<String>,
    /// Native Linux raw ELF assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_x86_64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_x86_64_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_x86_64_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_aarch64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_aarch64_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_aarch64_size: Option<u64>,
    /// The static bootstrap verifier published beside each native Linux executable
    /// (`aterm-bootstrap-verify-<version>-linux-<arch>`): its asset name, SHA-256 and size,
    /// signed with the rest of the manifest so install.sh's verifier pin can be bound to the
    /// release signer (`ship pin-verifier --from-release`). No client reads them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_verifier_x86_64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_verifier_x86_64_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_verifier_x86_64_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_verifier_aarch64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_verifier_aarch64_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_verifier_aarch64_size: Option<u64>,
    /// The install.sh interface those verifiers speak (`aterm_bootstrap_verify::pin::INTERFACE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_verifier_interface: Option<u32>,
    /// The Intel batteries-included DMG's asset name, e.g. `"aterm-0.47.0-x86_64.dmg"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmg_x86_64: Option<String>,
    /// SHA-256 (lowercase hex) of [`Self::dmg_x86_64`]'s bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmg_x86_64_sha256: Option<String>,
    /// Minimum macOS version the bundle declares (`LSMinimumSystemVersion`), e.g. `"11.0"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_os: Option<String>,
    /// Apple Team ID the release is signed under; `""` is the ad hoc tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// RFC3339 UTC publish time, e.g. `"2026-07-06T21:29:44Z"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pub_date: Option<String>,
    /// Optional operator apply floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_build: Option<u64>,
    /// The id of the machine that signed this release, e.g. `"m3"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// The `roster_seq` of the roster that authorized [`Self::machine_id`] at cut time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_seq: Option<u64>,
    /// The release's CHANGELOG.md section body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
}

impl Manifest {
    /// Select a native target only when all its signed identity fields agree.
    pub fn linux_artifact(
        &self,
        target: crate::linux::LinuxTarget,
    ) -> Result<Option<crate::linux::LinuxArtifact<'_>>, String> {
        let (name, digest, size) = match target {
            crate::linux::LinuxTarget::X86_64 => (
                self.linux_x86_64.as_deref(),
                self.linux_x86_64_sha256.as_deref(),
                self.linux_x86_64_size,
            ),
            crate::linux::LinuxTarget::Aarch64 => (
                self.linux_aarch64.as_deref(),
                self.linux_aarch64_sha256.as_deref(),
                self.linux_aarch64_size,
            ),
        };
        crate::linux::artifact(target, &self.version, name, digest, size)
    }

    /// Parse a manifest from TOML text, rejecting a schema this build is too old to understand.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Manifest = aterm_toml::from_str(text).map_err(|e| format!("parse manifest: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema > SUPPORTED_SCHEMA {
            return Err(format!(
                "manifest schema {} is newer than supported ({SUPPORTED_SCHEMA}); upgrade aterm",
                self.schema
            ));
        }
        if let Some(min_build) = self.min_build
            && min_build > self.build_number
        {
            return Err(format!(
                "manifest min_build {min_build} exceeds its build_number {}; refusing an \
                 impossible update floor",
                self.build_number
            ));
        }
        self.linux_artifact(crate::linux::LinuxTarget::X86_64)?;
        self.linux_artifact(crate::linux::LinuxTarget::Aarch64)?;
        Ok(())
    }

    /// Serialize to the published TOML shape.
    pub fn to_toml(&self) -> Result<String, String> {
        self.validate()?;
        if let Some(body) = &self.changelog {
            if body.contains("'''") {
                return Err(
                    "changelog contains ''' — unrepresentable inside a TOML multiline \
                     literal string; rewrite the offending CHANGELOG.md line"
                        .into(),
                );
            }
            if let Some(bad) = body
                .chars()
                .find(|c| c.is_control() && *c != '\n' && *c != '\t')
            {
                return Err(format!(
                    "changelog contains control character {bad:?} — TOML forbids it raw \
                     inside a literal string; rewrite the offending CHANGELOG.md line"
                ));
            }
        }
        let mut head = self.clone();
        head.changelog = None;
        let mut out =
            aterm_toml::to_string(&head).map_err(|e| format!("serialize manifest: {e}"))?;
        if let Some(body) = &self.changelog {
            out.push_str("changelog = '''\n");
            out.push_str(body);
            out.push_str("'''\n");
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
