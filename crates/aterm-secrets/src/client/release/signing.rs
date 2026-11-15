// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Automatic import and proof of the Developer ID pair the admitted vault holds, read in place
//! from the store: the pair is one set from one verified version, and no file under the home is
//! read.

use super::*;
use std::time::{Duration, Instant};

const RETRY: Duration = Duration::from_secs(60);
const RECHECK: Duration = Duration::from_secs(30 * 60);
const PROOF: &str = "release-signing-proof.toml";

/// Read-only, value-free readiness of this member's automatic signing setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningStatus {
    /// Signing does not apply to this platform or no vault is configured.
    NotApplicable(String),
    /// An authenticated pair or a current successful signing proof is still needed.
    Pending(String),
    /// This material signed successfully and its owned local keychain is unchanged.
    Ready(String),
}

struct ProofRecord {
    material: String,
    local: String,
    identity: String,
    proved: u64,
}

impl ProofRecord {
    fn parse(raw: &str) -> Option<Self> {
        let value: aterm_toml::Value = aterm_toml::from_str(raw).ok()?;
        Some(Self {
            material: value.get("material")?.as_str()?.into(),
            local: value.get("local")?.as_str()?.into(),
            identity: value.get("identity")?.as_str()?.into(),
            proved: value.get("proved")?.as_integer()?.try_into().ok()?,
        })
    }

    fn text(&self) -> String {
        // Each string is generated here from hexadecimal digests and decimal metadata.
        format!(
            "material = {:?}\nlocal = {:?}\nidentity = {:?}\nproved = {}\n",
            self.material, self.local, self.identity, self.proved
        )
    }
}

/// The SHA-256 of `bytes`, hex: the proof record's digests, spelled as older builds spelled them
/// so a record either build wrote keeps its meaning for the other.
fn hex_digest(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

fn now() -> crate::Result<u64> {
    aterm_time::unix_secs().ok_or_else(|| Error::new("signing proof clock is unavailable"))
}

/// Bind the proof to the private password and the keychain's current file identity. A
/// replaced or modified keychain requires another real signature, even within the TTL.
fn local_stamp(paths: &Paths) -> crate::Result<Option<String>> {
    use std::os::unix::fs::MetadataExt as _;
    let Some(password) =
        fsx::read_private(&crate::release_keychain::password_path(&paths.home), 1024)?
    else {
        return Ok(None);
    };
    let meta = match std::fs::symlink_metadata(crate::release_keychain::keychain_path(&paths.home))
    {
        Ok(meta)
            if meta.is_file()
                && meta.uid() == fsx::euid()
                && meta.nlink() == 1
                && meta.mode() & 0o077 == 0
                && meta.mode() & 0o6000 == 0 =>
        {
            meta
        }
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::new(format!("signing keychain metadata: {error}"))),
    };
    Ok(Some(format!(
        "{}:{}:{}:{}:{}:{}",
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        hex_digest(password.as_slice())
    )))
}

/// A successful proof for these exact authenticated bytes, and its age. Keychain upkeep
/// does not change which material signed; its current usability is checked separately.
fn material_proof(paths: &Paths, material: &Material) -> crate::Result<Option<(ProofRecord, u64)>> {
    let Some(raw) = fsx::read_private(&paths.dir.join(PROOF), 2048)? else {
        return Ok(None);
    };
    let Some(record) = raw.as_str().and_then(ProofRecord::parse) else {
        return Ok(None);
    };
    let Some(age) = now()?.checked_sub(record.proved) else {
        return Ok(None);
    };
    Ok(
        (record.material == material.stamp && record.identity == material.identity)
            .then_some((record, age)),
    )
}

fn proof_current(paths: &Paths, material: &Material) -> crate::Result<bool> {
    let Some((record, age)) = material_proof(paths, material)? else {
        return Ok(false);
    };
    Ok(age < RECHECK.as_secs() && local_stamp(paths)?.as_deref() == Some(record.local.as_str()))
}

/// Read readiness without modifying credentials, keychains, or the roster's anti-rollback
/// floor. File presence alone never returns Ready.
pub fn signing_status(paths: &Paths) -> crate::Result<SigningStatus> {
    if !cfg!(target_os = "macos") {
        return Ok(SigningStatus::NotApplicable(
            "Developer ID signing applies on macOS".into(),
        ));
    }
    let Some(config) = config(paths)? else {
        return Ok(SigningStatus::NotApplicable(
            "no vault is configured".into(),
        ));
    };
    if !alab_vault(&config) && read_verified_entry(paths, &config, RELEASE_PROFILE)?.is_none() {
        return Ok(SigningStatus::NotApplicable(
            "this vault has no shared release ring".into(),
        ));
    }
    status_authorized(paths, &config, authorized_ring_readonly(paths))
}

fn status_authorized(
    paths: &Paths,
    config: &Config,
    authorized: bool,
) -> crate::Result<SigningStatus> {
    if !authorized {
        return Ok(SigningStatus::Pending(
            "awaiting the store's matching release profile/key and current master-signed roster"
                .into(),
        ));
    }
    status_with(paths, config)
}

fn status_with(paths: &Paths, config: &Config) -> crate::Result<SigningStatus> {
    let Some(material) = material(paths, config)? else {
        return Ok(SigningStatus::Pending(
            "awaiting an authorized matching Developer ID certificate and key".into(),
        ));
    };
    if proof_current(paths, &material)? {
        Ok(SigningStatus::Ready(format!(
            "{}: private keychain and unattended signature proved",
            material.identity
        )))
    } else {
        Ok(SigningStatus::Pending("authorized signing material held; automatic keychain repair and signature proof pending".into()))
    }
}

/// Converge the existing authenticated pair without creating any new signing identity.
/// Busy transactions are retried by the next ordinary convergence/subscriber pass.
pub fn sync_signing(paths: &Paths) -> crate::Result<String> {
    if !cfg!(target_os = "macos") {
        return Ok("Developer ID signing applies on macOS".into());
    }
    let config = config(paths)?.ok_or_else(|| Error::new("no vault is configured"))?;
    if !authorized_ring(paths) {
        return Err(Error::new(
            "awaiting an authorized release ring; automatic setup will retry",
        ));
    }
    let line = Rounds::default()
        .step(paths, &config, &prepare)?
        .ok_or_else(|| Error::new("signing proof not recorded"))?;
    if !matches!(signing_status(paths)?, SigningStatus::Ready(_)) {
        return Err(Error::new(line));
    }
    Ok(line)
}

#[derive(Debug, Default)]
pub(super) struct Rounds {
    proved: Option<(String, Instant)>,
    retry: Option<Instant>,
    said: Option<String>,
}

struct Material {
    cert: crate::SecretBytes,
    key: crate::SecretBytes,
    identity: String,
    stamp: String,
}

fn profile_identity(paths: &Paths, config: &Config) -> crate::Result<Option<String>> {
    let Some(profile) = read_verified_entry(paths, config, RELEASE_PROFILE)? else {
        return Ok(None);
    };
    let text = profile
        .value()
        .as_str()
        .ok_or_else(|| Error::new("release profile is not UTF-8"))?;
    let value: aterm_toml::Value =
        aterm_toml::from_str(text).map_err(|_| Error::new("release profile is malformed"))?;
    match value.get("signing_identity_sha1") {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|id| aterm_codec::hex::is_hex(id, 40))
            .map(|id| Some(id.to_ascii_uppercase()))
            .ok_or_else(|| Error::new("release profile's Apple identity is malformed")),
    }
}

/// SHA-1 is only Apple's certificate identifier; trust is checked by the platform after
/// import. The private key and its signing authorization use their own cryptography.
fn certificate_identity(bytes: &[u8]) -> crate::Result<String> {
    let der = if bytes.starts_with(b"-----BEGIN CERTIFICATE-----") {
        let text =
            std::str::from_utf8(bytes).map_err(|_| Error::new("certificate PEM is malformed"))?;
        let body = text
            .strip_prefix("-----BEGIN CERTIFICATE-----")
            .and_then(|s| s.split_once("-----END CERTIFICATE-----"))
            .ok_or_else(|| Error::new("certificate PEM is incomplete"))?
            .0;
        let encoded: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        aterm_codec::base64::decode_strict(encoded.as_bytes())
            .map_err(|_| Error::new("certificate PEM is malformed"))?
    } else {
        bytes.to_vec()
    };
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &der);
    Ok(aterm_codec::hex::encode_lower(digest.as_ref()).to_ascii_uppercase())
}

fn material(paths: &Paths, config: &Config) -> crate::Result<Option<Material>> {
    let wanted = profile_identity(paths, config)?;
    let mut shared = None;
    // The candidates are the certificate names the verified view holds; each pair is read as one
    // set from one version, in place.
    let names = super::super::entry::held_names(paths, config)?.unwrap_or_default();
    for name in &names {
        if !name.starts_with(crate::entry_class::APPLE_PREFIX) || !name.ends_with(".cer") {
            continue;
        }
        let key_name = format!("{}.key", name.strip_suffix(".cer").unwrap_or(name));
        if !crate::entry_class::signing_material(name)
            || !crate::entry_class::signing_material(&key_name)
        {
            continue;
        }
        let Some(pair) = read_verified_set(paths, config, &[name.as_str(), key_name.as_str()])?
        else {
            continue;
        };
        let mut pair = pair.into_iter().map(|entry| entry.into_parts().0);
        let (Some(cert), Some(key)) = (pair.next(), pair.next()) else {
            continue;
        };
        let identity = certificate_identity(cert.as_slice())?;
        let stamp = format!(
            "{}:{}",
            hex_digest(cert.as_slice()),
            hex_digest(key.as_slice())
        );
        let pair = Material {
            cert,
            key,
            identity,
            stamp,
        };
        if wanted
            .as_ref()
            .is_none_or(|wanted| wanted == &pair.identity)
        {
            return Ok(Some(pair));
        }
        if shared.is_none() {
            shared = Some(pair);
        }
    }
    Ok(shared)
}

/// The previously proven, currently authorized Apple identity for the store's release profile.
/// Keychain upkeep or an expired readiness receipt does not select a different identity: the
/// cutter still checks its keychain and signs with it. This grants no current readiness. A
/// profile the operator names by file is not the store's and never adopts this member's shared
/// identity, so the cutter asks this only of the store's.
pub fn effective_signing_identity(paths: &Paths) -> crate::Result<Option<String>> {
    effective_identity_with(paths, authorized_ring_readonly(paths))
}

/// The same selection under a fixture's roster anchors, with real ring authorization.
/// This test seam is absent from production release builds.
#[cfg(any(test, debug_assertions))]
pub fn test_signing_observation(
    paths: &Paths,
    anchors: &[&str],
    at: i64,
) -> crate::Result<(Option<String>, SigningStatus)> {
    let authorized = authorized_ring_check(paths, anchors, at, false);
    let config = config(paths)?.ok_or_else(|| Error::new("fixture has no vault"))?;
    Ok((
        effective_identity_with(paths, authorized)?,
        status_authorized(paths, &config, authorized)?,
    ))
}

fn effective_identity_with(paths: &Paths, authorized: bool) -> crate::Result<Option<String>> {
    if !authorized {
        return Ok(None);
    }
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let Some(material) = material(paths, &config)? else {
        return Ok(None);
    };
    Ok(material_proof(paths, &material)?.map(|_| material.identity))
}

/// Select current authenticated shared material for the explicit probe's real signature.
/// This is identity selection only; it does not claim that the keychain is ready.
pub(super) fn probe_identity(paths: &Paths) -> crate::Result<String> {
    let config = config(paths)?.ok_or_else(|| Error::new("release probe has no vault"))?;
    if !authorized_ring_readonly(paths) {
        return Err(Error::new("release probe has no current release ring"));
    }
    material(paths, &config)?
        .map(|material| material.identity)
        .ok_or_else(|| Error::new("release probe awaits current authenticated signing material"))
}

/// Called only after the explicit probe verified a real signature while still holding the
/// shared SigningLease. Publish the same receipt contract used by automatic convergence.
pub(super) fn record_probe_signature(paths: &Paths, identity: &str) -> crate::Result<()> {
    let config = config(paths)?.ok_or_else(|| Error::new("release probe has no vault"))?;
    if !authorized_ring_readonly(paths) {
        return Err(Error::new("release probe ring changed before its receipt"));
    }
    let material = material(paths, &config)?
        .filter(|material| material.identity == identity)
        .ok_or_else(|| Error::new("release probe material changed before its receipt"))?;
    write_proof(paths, &material)
}

fn write_proof(paths: &Paths, material: &Material) -> crate::Result<()> {
    let record = ProofRecord {
        material: material.stamp.clone(),
        local: local_stamp(paths)?
            .ok_or_else(|| Error::new("automatic signing proof left no owned private keychain"))?,
        identity: material.identity.clone(),
        proved: now()?,
    };
    fsx::write_atomic(&paths.dir.join(PROOF), record.text().as_bytes(), 0o600)
}

/// Hand the pair to `security import`, which reads only files: the one place a vault value is on
/// disk, a private temporary directory outside the home that is removed when this returns.
fn prepare(paths: &Paths, material: &Material) -> crate::Result<()> {
    let dir = aterm_tempfile::tempdir()
        .map_err(|error| Error::new(format!("stage shared signing material: {error}")))?;
    let cert = dir.path().join("certificate.cer");
    let key = dir.path().join("private.key");
    fsx::write_atomic(&cert, material.cert.as_slice(), 0o600)?;
    fsx::write_atomic(&key, material.key.as_slice(), 0o600)?;
    crate::release_keychain::prepare_existing_pair(&paths.home, &cert, &key, &material.identity)
        .map_err(|refused| Error::new(refused.what))
}

impl Rounds {
    pub(super) fn after_round(&mut self, paths: &Paths, config: &Config, log: &dyn Fn(&str)) {
        if !cfg!(target_os = "macos") || self.retry.is_some_and(|at| at.elapsed() < RETRY) {
            return;
        }
        if !authorized_ring(paths) {
            return;
        }
        let outcome = self.step(paths, config, &prepare);
        match outcome {
            Ok(None) => {}
            Ok(Some(line)) => {
                self.retry = None;
                if self.said.as_ref() != Some(&line) {
                    log(&line);
                }
                self.said = Some(line);
            }
            Err(error) => {
                self.retry = Some(Instant::now());
                let line = format!("shared signing setup will retry: {error}");
                if self.said.as_ref() != Some(&line) {
                    log(&line);
                }
                self.said = Some(line);
            }
        }
    }

    fn step(
        &mut self,
        paths: &Paths,
        config: &Config,
        prepare: &dyn Fn(&Paths, &Material) -> crate::Result<()>,
    ) -> crate::Result<Option<String>> {
        let Some(material) = material(paths, config)? else {
            return Ok(Some(
                "shared signing setup awaits an authorized matching certificate and key".into(),
            ));
        };
        if proof_current(paths, &material)?
            && self
                .proved
                .as_ref()
                .is_some_and(|(stamp, at)| stamp == &material.stamp && at.elapsed() < RECHECK)
        {
            return Ok(None);
        }
        let _lease = crate::release_keychain::signing_lease(&paths.home).map_err(Error::new)?;
        // Invalidate a prior receipt before a repair can change the keychain.
        fsx::write_atomic(
            &paths.dir.join(PROOF),
            b"# automatic signature proof pending\n",
            0o600,
        )?;
        prepare(paths, &material)?;
        write_proof(paths, &material)?;
        self.proved = Some((material.stamp, Instant::now()));
        Ok(Some(format!(
            "shared signing identity {} is ready (private keychain, unattended signing proved)",
            material.identity
        )))
    }
}

#[cfg(test)]
#[path = "signing_tests.rs"]
mod tests;
