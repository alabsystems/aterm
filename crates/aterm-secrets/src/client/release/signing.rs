// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Automatic import and proof of the Developer ID pair delivered by the admitted vault.

use super::*;
use std::time::{Duration, Instant};

const RETRY: Duration = Duration::from_secs(60);
const RECHECK: Duration = Duration::from_secs(30 * 60);
const APPLE_PREFIX: &str = "~/.aterm/apple/devid-";
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
        Ok(meta) if meta.is_file() && meta.uid() == fsx::euid() && meta.nlink() == 1 => meta,
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
        materialize::digest_hex(password.as_slice())
    )))
}

fn proof_current(paths: &Paths, material: &Material) -> crate::Result<bool> {
    let Some(raw) = fsx::read_private(&paths.dir.join(PROOF), 2048)? else {
        return Ok(false);
    };
    let Some(record) = raw.as_str().and_then(ProofRecord::parse) else {
        return Ok(false);
    };
    let age = now()?.checked_sub(record.proved);
    Ok(record.material == material.stamp
        && record.identity == material.identity
        && age.is_some_and(|age| age < RECHECK.as_secs())
        && local_stamp(paths)?.as_deref() == Some(record.local.as_str()))
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
    if !alab_vault(&config)
        && !paths.home.join(RELEASE_PROFILE).exists()
        && !paths.home.join(LEGACY_RELEASE_PROFILE).exists()
    {
        return Ok(SigningStatus::NotApplicable(
            "this vault has no shared release ring".into(),
        ));
    }
    if !authorized_ring_readonly(paths) {
        return Ok(SigningStatus::Pending(
            "awaiting a private matching release profile/key and current master-signed roster"
                .into(),
        ));
    }
    status_with(paths, &config)
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
        Ok(SigningStatus::Pending("authorized signing files present; automatic keychain repair and signature proof pending".into()))
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

fn profile_identity(paths: &Paths) -> crate::Result<Option<String>> {
    let profile = match fsx::read_private(&paths.home.join(RELEASE_PROFILE), MAX_READ)? {
        Some(profile) => Some(profile),
        None => fsx::read_private(&paths.home.join(LEGACY_RELEASE_PROFILE), MAX_READ)?,
    };
    let Some(profile) = profile else {
        return Ok(None);
    };
    let text = profile
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
    let state = load_state(paths)?;
    let wanted = profile_identity(paths)?;
    let mut shared = None;
    for name in state.manifest.files.keys() {
        if !name.starts_with(APPLE_PREFIX) || !name.ends_with(".cer") {
            continue;
        }
        let key_name = format!("{}.key", name.trim_end_matches(".cer"));
        if !state.manifest.files.contains_key(&key_name) {
            continue;
        }
        let Some(cert) = read_verified_entry(paths, config, name)? else {
            continue;
        };
        let Some(key) = read_verified_entry(paths, config, &key_name)? else {
            continue;
        };
        let identity = certificate_identity(cert.value().as_slice())?;
        let cert = cert.value().clone();
        let key = key.value().clone();
        let stamp = format!(
            "{}:{}",
            materialize::digest_hex(cert.as_slice()),
            materialize::digest_hex(key.as_slice())
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

/// The proven, vault-delivered Apple identity for this exact local release profile. This
/// leaves the release ring's cryptographic identity and the profile's bytes unchanged.
/// An explicit unrelated profile never adopts this member's shared Apple identity.
pub fn effective_signing_identity(paths: &Paths, profile: &Path) -> crate::Result<Option<String>> {
    effective_identity_with(paths, profile, authorized_ring_readonly(paths))
}

fn effective_identity_with(
    paths: &Paths,
    profile: &Path,
    authorized: bool,
) -> crate::Result<Option<String>> {
    let selected = if paths.home.join(RELEASE_PROFILE).exists() {
        paths.home.join(RELEASE_PROFILE)
    } else {
        paths.home.join(LEGACY_RELEASE_PROFILE)
    };
    let same_profile = std::fs::canonicalize(profile)
        .ok()
        .zip(std::fs::canonicalize(selected).ok())
        .is_some_and(|(a, b)| a == b);
    if !same_profile || !authorized {
        return Ok(None);
    }
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    let Some(material) = material(paths, &config)? else {
        return Ok(None);
    };
    Ok(proof_current(paths, &material)?.then_some(material.identity))
}

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
        let record = ProofRecord {
            material: material.stamp.clone(),
            local: local_stamp(paths)?.ok_or_else(|| {
                Error::new("automatic signing proof left no owned private keychain")
            })?,
            identity: material.identity.clone(),
            proved: now()?,
        };
        let raw = record.text();
        fsx::write_atomic(&paths.dir.join(PROOF), raw.as_bytes(), 0o600)?;
        self.proved = Some((material.stamp, Instant::now()));
        Ok(Some(format!(
            "shared signing identity {} is ready (private keychain, unattended signing proved)",
            material.identity
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn verified_pairs_are_prepared_and_missing_local_state_is_repaired() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("member"));
        let source = root.path().join("vault");
        test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        let cfg = config(&paths).unwrap().unwrap();
        let admin = admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            "~/.aterm/apple/devid-shared.cer",
            b"synthetic certificate",
        )
        .unwrap();
        doc.set_entry(
            &Audience::Everyone,
            "~/.aterm/apple/devid-shared.key",
            b"synthetic private key",
        )
        .unwrap();
        admin_publish(&paths, &admin, &doc).unwrap();
        let calls = Cell::new(0);
        let prepare = |paths: &Paths, material: &Material| {
            calls.set(calls.get() + 1);
            assert_eq!(material.key.as_slice(), b"synthetic private key");
            let keychain = crate::release_keychain::keychain_path(&paths.home);
            std::fs::create_dir_all(keychain.parent().unwrap()).unwrap();
            fsx::write_atomic(&keychain, b"prepared", 0o600).unwrap();
            let password = crate::release_keychain::password_path(&paths.home);
            std::fs::create_dir_all(password.parent().unwrap()).unwrap();
            fsx::write_atomic(&password, b"prepared", 0o600).unwrap();
            Ok(())
        };
        let mut rounds = Rounds::default();
        let profile = paths.home.join(RELEASE_PROFILE);
        fsx::ensure_private_dir(profile.parent().unwrap()).unwrap();
        let original_profile =
            b"signing_identity_sha1 = \"0000000000000000000000000000000000000000\"\n";
        fsx::write_atomic(&profile, original_profile, 0o600).unwrap();
        assert!(matches!(
            status_with(&paths, &cfg).unwrap(),
            SigningStatus::Pending(_)
        ));
        assert!(
            rounds
                .step(&paths, &cfg, &prepare)
                .unwrap()
                .unwrap()
                .contains("proved")
        );
        assert!(matches!(
            status_with(&paths, &cfg).unwrap(),
            SigningStatus::Ready(_)
        ));
        assert!(
            effective_identity_with(&paths, &profile, true)
                .unwrap()
                .is_some()
        );
        assert!(
            effective_identity_with(&paths, &profile, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read(&profile).unwrap(), original_profile);
        assert!(rounds.step(&paths, &cfg, &prepare).unwrap().is_none());
        std::fs::remove_file(crate::release_keychain::password_path(&paths.home)).unwrap();
        assert!(
            effective_identity_with(&paths, &profile, true)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            status_with(&paths, &cfg).unwrap(),
            SigningStatus::Pending(_)
        ));
        assert!(rounds.step(&paths, &cfg, &prepare).unwrap().is_some());
        assert_eq!(calls.get(), 2);
        fsx::write_atomic(
            &crate::release_keychain::keychain_path(&paths.home),
            b"changed",
            0o600,
        )
        .unwrap();
        assert!(matches!(
            status_with(&paths, &cfg).unwrap(),
            SigningStatus::Pending(_)
        ));
        let fail = |_: &Paths, _: &Material| Err(Error::new("synthetic repair failure"));
        assert!(rounds.step(&paths, &cfg, &fail).is_err());
        assert!(matches!(
            status_with(&paths, &cfg).unwrap(),
            SigningStatus::Pending(_)
        ));
        // Recipient exclusion is authoritative even when an old successful receipt remains.
        let admin = admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine(
            "other-member",
            &crate::age::Identity::generate()
                .unwrap()
                .recipient()
                .to_bech32(),
        )
        .unwrap();
        doc.set_entry(
            &Audience::Hosts(vec!["other-member".into()]),
            "~/.aterm/apple/devid-shared.key",
            b"synthetic private key",
        )
        .unwrap();
        doc.unset_entry(&Audience::Everyone, "~/.aterm/apple/devid-shared.key")
            .unwrap();
        admin_publish(&paths, &admin, &doc).unwrap();
        assert!(material(&paths, &cfg).unwrap().is_none());
        assert!(matches!(
            status_with(&paths, &cfg).unwrap(),
            SigningStatus::Pending(_)
        ));
    }
}
