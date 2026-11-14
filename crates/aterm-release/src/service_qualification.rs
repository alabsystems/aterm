// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Release-time qualification of independently activated service images.

#[cfg(target_os = "macos")]
use crate::ledger::GitCli;
use crate::ledger::{Error, Result};
#[cfg(any(target_os = "macos", test))]
use aterm_update_core::service_image::ProtocolRange;
use aterm_update_core::service_image::{Qualification, ServiceDeclaration, image_digest};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Published report whose exact bytes the signed appcast binds.
pub const REPORT_ASSET: &str = "aterm-service-qualification.json";

/// Every new service-capable release carries exactly one qualification report.
pub fn validate_assets(
    names: &[String],
    version: &str,
    signed: bool,
    rostered: bool,
    handoff: &[String],
) -> Result<()> {
    if names
        .iter()
        .filter(|name| name.as_str() == REPORT_ASSET)
        .count()
        != 1
    {
        return Err(Error::new(
            "release must carry exactly one service qualification report",
        ));
    }
    let base: Vec<_> = names
        .iter()
        .filter(|name| name.as_str() != REPORT_ASSET)
        .cloned()
        .collect();
    crate::channel::validate_channel_asset_set(&base, version, signed, rostered, handoff)
}

#[cfg(test)]
pub(crate) fn fixture(image: &Path, build: u64, revision: &str, manifest: &Path) {
    emit(
        image,
        build,
        revision,
        revision,
        &"c".repeat(64),
        "test fixture",
        ProtocolRange { min: 1, max: 1 },
        manifest,
    )
    .unwrap();
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg(any(target_os = "macos", test))]
struct ProtocolReply {
    schema: u32,
    min: u32,
    max: u32,
}

#[derive(Serialize, Deserialize)]
struct Extension {
    service: ServiceDeclaration,
}

fn sha(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

fn valid_sha256(value: &str) -> bool {
    aterm_codec::hex::is_lower(value, 64)
}

fn validate_declaration(service: &ServiceDeclaration) -> Result<()> {
    if service.schema != 1
        || service.platform != "universal-apple-darwin"
        || service.executable != std::path::Path::new("Contents/MacOS/aterm")
        || !service.protocol.supports(&[])
        || !valid_sha256(&service.image_sha256)
        || !valid_sha256(&service.executable_sha256)
        || !valid_sha256(&service.qualification_sha256)
    {
        return Err(Error::new(
            "invalid service declaration in appcast manifest",
        ));
    }
    Ok(())
}

/// Qualify the exact signed image after notarization, then bind its report into the
/// appcast before the appcast's detached signature is made. `source_revision` names
/// the source whose release checks ran; `revision` includes its reserved ledger claim.
#[cfg(target_os = "macos")]
#[expect(
    clippy::too_many_arguments,
    reason = "the qualification binds the exact release identities and image paths"
)]
pub fn produce(
    repo: &Path,
    source_revision: &str,
    verified_input_sha256: &str,
    image: &Path,
    dmg: &Path,
    build: u64,
    revision: &str,
    manifest: &Path,
) -> Result<PathBuf> {
    let (qualified, evidence) =
        crate::gates::verified(&GitCli::new(repo), source_revision, verified_input_sha256)?;
    if !qualified {
        return Err(Error::new(format!(
            "service image has no release qualification: {evidence}"
        )));
    }
    crate::publish::selfcheck_signing(
        aterm_update_core::pins::APPLE_TEAM_ID,
        image,
        dmg,
        &crate::sign::RealAppleTools,
    )?;
    let before = image_digest(image).map_err(Error::new)?;
    let executable = image.join("Contents/MacOS/aterm");
    let mut cmd = aterm_uds::spawnfd::command(&executable);
    cmd.args(["secrets", "service-repair", "protocol"]);
    let output = aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60))
        .map_err(|e| Error::new(format!("service protocol probe: {e}")))?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "service protocol probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let reply = parse_protocol(&output.stdout)?;
    if image_digest(image).map_err(Error::new)? != before {
        return Err(Error::new("service image changed during qualification"));
    }
    emit(
        image,
        build,
        revision,
        source_revision,
        verified_input_sha256,
        &evidence,
        reply,
        manifest,
    )
}

/// A macOS bundle cannot be independently qualified on another host OS.
#[cfg(not(target_os = "macos"))]
#[expect(
    clippy::too_many_arguments,
    reason = "same interface as the macOS producer"
)]
pub fn produce(
    _repo: &Path,
    _source_revision: &str,
    _verified_input_sha256: &str,
    _image: &Path,
    _dmg: &Path,
    _build: u64,
    _revision: &str,
    _manifest: &Path,
) -> Result<PathBuf> {
    Err(Error::new("service image qualification requires macOS"))
}

#[cfg(any(target_os = "macos", test))]
fn parse_protocol(bytes: &[u8]) -> Result<ProtocolRange> {
    if bytes.len() > 4096 {
        return Err(Error::new("oversized service protocol reply"));
    }
    let reply: ProtocolReply = aterm_json::from_slice(bytes)
        .map_err(|e| Error::new(format!("service protocol reply: {e}")))?;
    let protocol = ProtocolRange {
        min: reply.min,
        max: reply.max,
    };
    if reply.schema != 1 || !protocol.supports(&[]) {
        return Err(Error::new("invalid service protocol declaration"));
    }
    Ok(protocol)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the report binds the exact release identities and evidence"
)]
#[cfg(any(target_os = "macos", test))]
fn emit(
    image: &Path,
    build: u64,
    revision: &str,
    source_revision: &str,
    verified_input_sha256: &str,
    evidence: &str,
    protocol: ProtocolRange,
    manifest_path: &Path,
) -> Result<PathBuf> {
    let executable_sha256 =
        aterm_update_core::sha256_file(&image.join("Contents/MacOS/aterm")).map_err(Error::new)?;
    let image_sha256 = image_digest(image).map_err(Error::new)?;
    let report = Qualification {
        schema: 1,
        status: "passed".into(),
        scope: "release-gate-and-signed-image-protocol".into(),
        build,
        source_revision: revision.into(),
        verified_source_revision: source_revision.into(),
        verified_input_sha256: verified_input_sha256.into(),
        release_check_evidence: evidence.into(),
        image_sha256: image_sha256.clone(),
        executable_sha256: executable_sha256.clone(),
        protocol: protocol.clone(),
        protocol_probe_passed: true,
        native_signature_verified: true,
        live_service_health_tested: false,
    };
    let bytes = aterm_json::to_vec(&report).map_err(|e| Error::new(e.to_string()))?;
    let extension = Extension {
        service: ServiceDeclaration {
            schema: 1,
            platform: "universal-apple-darwin".into(),
            image_sha256,
            executable: "Contents/MacOS/aterm".into(),
            executable_sha256,
            qualification_sha256: sha(&bytes),
            protocol,
        },
    };
    validate_declaration(&extension.service)?;
    report
        .validate(build, revision, &extension.service)
        .map_err(Error::new)?;
    let mut manifest = crate::durable::read_regular_text(manifest_path, 1024 * 1024)
        .map_err(|e| Error::new(e.to_string()))?;
    let identity = aterm_update_core::Manifest::parse(&manifest).map_err(Error::new)?;
    if identity.build_number != build || identity.commit.as_deref() != Some(revision) {
        return Err(Error::new(
            "service qualification does not match appcast identity",
        ));
    }
    if aterm_toml::from_str::<Extension>(&manifest).is_ok() {
        return Err(Error::new(
            "service qualification is already bound into this appcast",
        ));
    }
    manifest.push('\n');
    manifest.push_str(&aterm_toml::to_string(&extension).map_err(|e| Error::new(e.to_string()))?);
    let roundtrip: Extension =
        aterm_toml::from_str(&manifest).map_err(|e| Error::new(e.to_string()))?;
    if roundtrip.service != extension.service {
        return Err(Error::new("service appcast extension did not round trip"));
    }
    let report_path = manifest_path
        .parent()
        .ok_or_else(|| Error::new("appcast has no parent"))?
        .join(REPORT_ASSET);
    crate::durable::write_atomic_durable(&report_path, &bytes, 0o644)
        .map_err(|e| Error::new(e.to_string()))?;
    crate::durable::write_atomic_durable(manifest_path, manifest.as_bytes(), 0o644)
        .map_err(|e| Error::new(e.to_string()))?;
    Ok(report_path)
}

/// Recheck a persisted qualification before upload or a resumed cut.
pub fn verify(image: &Path, manifest_path: &Path) -> Result<()> {
    let service = verify_published_report(manifest_path)?;
    if image_digest(image).map_err(Error::new)? != service.image_sha256
        || aterm_update_core::sha256_file(&image.join("Contents/MacOS/aterm"))
            .map_err(Error::new)?
            != service.executable_sha256
    {
        return Err(Error::new(
            "service qualification does not bind the local release image",
        ));
    }
    Ok(())
}

/// Read the report digest only from a manifest whose signature the caller verified.
pub fn report_digest(manifest: &[u8]) -> Result<String> {
    if manifest.len() > 1024 * 1024 {
        return Err(Error::new("oversized appcast manifest"));
    }
    let text = std::str::from_utf8(manifest).map_err(|e| Error::new(e.to_string()))?;
    let extension: Extension = aterm_toml::from_str(text).map_err(|e| Error::new(e.to_string()))?;
    validate_declaration(&extension.service)?;
    Ok(extension.service.qualification_sha256)
}

/// Validate a published snapshot's signed report without executing or reconstructing
/// an image. The publication recovery separately verifies its signed ZIP/DMG digests.
pub fn verify_published_report(manifest_path: &Path) -> Result<ServiceDeclaration> {
    let text = crate::durable::read_regular_text(manifest_path, 1024 * 1024)
        .map_err(|e| Error::new(e.to_string()))?;
    let manifest = aterm_update_core::Manifest::parse(&text).map_err(Error::new)?;
    let extension: Extension = aterm_toml::from_str(&text)
        .map_err(|e| Error::new(format!("missing or invalid service qualification: {e}")))?;
    validate_declaration(&extension.service)?;
    let path = manifest_path
        .parent()
        .ok_or_else(|| Error::new("appcast has no parent"))?
        .join(REPORT_ASSET);
    let bytes = crate::durable::read_regular_bytes(&path, 1024 * 1024)
        .map_err(|e| Error::new(e.to_string()))?;
    let report: Qualification =
        aterm_json::from_slice(&bytes).map_err(|e| Error::new(e.to_string()))?;
    report
        .validate(
            manifest.build_number,
            manifest.commit.as_deref().unwrap_or(""),
            &extension.service,
        )
        .map_err(Error::new)?;
    if sha(&bytes) != extension.service.qualification_sha256 {
        return Err(Error::new(
            "service qualification does not bind the release image and appcast",
        ));
    }
    Ok(extension.service)
}

#[cfg(test)]
#[path = "service_qualification_tests.rs"]
mod tests;
