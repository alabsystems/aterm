// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retain only a confirmed refresh rejection, never a token or permission to
//! authenticate. Current pool/root authority remains independently required.

use super::*;
use std::os::unix::fs::MetadataExt as _;

const MAX_BYTES: usize = 4096;

pub(super) fn path(home: &Path, region: Region) -> PathBuf {
    home.join(format!("{}.aterm-rejected.json", oauth_key(region)))
}

fn digest(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

pub(super) fn save(
    home: &Path,
    target: Target<'_>,
    config: &FileEvidence,
    credential: &FileEvidence,
) -> Result<(), NativeError> {
    config.check_current()?;
    credential.check_current()?;
    let marker = aterm_json::json!({
        "schema": 1,
        "issuer": issuer(target.region),
        "expected_email": target.expected,
        "config": digest(config.bytes.expose()),
        "rejected": digest(credential.bytes.expose()),
        "tombstone": digest(REVOKED),
    });
    let bytes = aterm_json::to_vec(&marker).map_err(|_| NativeError::Temporary)?;
    let path = path(home, target.region);
    aterm_secrets::fsx::ensure_private_dir(path.parent().ok_or(NativeError::Temporary)?)
        .map_err(|_| NativeError::Temporary)?;
    aterm_secrets::fsx::write_atomic(&path, &bytes, 0o600).map_err(|_| NativeError::Temporary)
}

pub(super) fn matches(home: &Path, target: Target<'_>) -> Result<bool, NativeError> {
    Ok(snapshot(home, target)?.is_some())
}

pub(super) fn snapshot(home: &Path, target: Target<'_>) -> Result<Option<Snapshot>, NativeError> {
    check_directory(home)?;
    let parent = home.join("oauth");
    match std::fs::symlink_metadata(&parent) {
        Ok(meta)
            if meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.uid() == aterm_uds::peer::our_uid()
                && meta.mode() & 0o022 == 0 => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        _ => return Err(NativeError::Temporary),
    }
    let path = path(home, target.region);
    let Some(bytes) =
        aterm_secrets::fsx::read_private(&path, MAX_BYTES).map_err(|_| NativeError::Temporary)?
    else {
        return Ok(None);
    };
    let value: Value = match aterm_json::from_slice(bytes.as_slice()) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    if value["schema"].as_u64() != Some(1)
        || value["issuer"].as_str() != Some(issuer(target.region))
        || !value["expected_email"]
            .as_str()
            .is_some_and(|email| accounts::email_matches(email, target.expected))
        || value["tombstone"].as_str() != Some(digest(REVOKED).as_str())
    {
        return Ok(None);
    }
    check_directory(&home.join("credentials"))?;
    let Some(config) = FileEvidence::read(home.join("config.toml"), false)? else {
        return Ok(None);
    };
    check_config(config.bytes.expose(), target.region)?;
    let Some(credential) = FileEvidence::read(credential_path(home, target.region), true)? else {
        return Ok(None);
    };
    let current = digest(credential.bytes.expose());
    let matched = value["config"].as_str() == Some(digest(config.bytes.expose()).as_str())
        && (value["rejected"].as_str() == Some(current.as_str())
            || value["tombstone"].as_str() == Some(current.as_str()));
    config.check_current()?;
    credential.check_current()?;
    // A replaced marker cannot lend its earlier decision to this snapshot.
    let kept =
        aterm_secrets::fsx::read_private(&path, MAX_BYTES).map_err(|_| NativeError::Temporary)?;
    if kept.as_ref().map(|kept| kept.as_slice()) != Some(bytes.as_slice()) {
        return Err(NativeError::Temporary);
    }
    if !matched {
        return Ok(None);
    }
    Ok(Some(Snapshot {
        home: home.to_owned(),
        config,
        credential,
        marker: bytes,
        marker_path: path,
    }))
}

/// Local rejection evidence only. Independent current authority is still needed
/// before a new sign-in; none of these bytes is a provider identity proof.
pub(in crate::harness) struct Snapshot {
    home: PathBuf,
    config: FileEvidence,
    credential: FileEvidence,
    marker: aterm_secrets::SecretBytes,
    marker_path: PathBuf,
}

impl Snapshot {
    pub(in crate::harness) fn check_current(&self) -> Result<(), String> {
        check_directory(&self.home)?;
        check_directory(&self.home.join("credentials"))?;
        let parent = std::fs::symlink_metadata(self.home.join("oauth"))
            .map_err(|_| "Kimi rejection directory is unavailable")?;
        if !parent.is_dir()
            || parent.file_type().is_symlink()
            || parent.uid() != aterm_uds::peer::our_uid()
            || parent.mode() & 0o022 != 0
        {
            return Err("Kimi rejection directory changed".into());
        }
        self.config.check_current()?;
        self.credential.check_current()?;
        let marker = aterm_secrets::fsx::read_private(&self.marker_path, MAX_BYTES)
            .map_err(|_| "Kimi rejection record is unavailable")?;
        if marker.as_ref().map(|bytes| bytes.as_slice()) != Some(self.marker.as_slice()) {
            return Err("Kimi rejection record changed".into());
        }
        Ok(())
    }
    pub(in crate::harness) fn recovery_key(
        &self,
        parent: login_drive::RecoveryKey,
    ) -> login_drive::RecoveryKey {
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm.kimi-reauthentication.v1\0");
        digest.update(parent.0);
        for bytes in [
            self.home.as_os_str().as_encoded_bytes(),
            self.config.bytes.expose(),
            self.credential.bytes.expose(),
            self.marker.as_slice(),
        ] {
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
        login_drive::RecoveryKey(digest.finalize())
    }
}
