// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A persistent volume witness supplements, rather than rewrites, legacy directory custody.

use super::{ANCHOR_LEN, CLAIM_LEN, WITNESS_AT};
use crate::{Error, Result};
use aterm_uds::{
    directory_identity::Identity,
    private_file::{self, PrivateFileError},
};
use std::path::Path;

const DOMAIN: &[u8] = b"aterm/commons/table-volume/1\0";
const MAGIC: &[u8; 5] = b"ATVW1";
const LEN: usize = 5 + 32 + 16;

fn name(claim_name: &str) -> String {
    format!("{claim_name}.volume")
}

fn anchor(claim: &[u8]) -> [u8; 32] {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(DOMAIN);
    hash.update(&claim[..WITNESS_AT]);
    hash.finalize()
}

pub(super) fn witness(dir: &Path, claim_name: &str) -> Result<Option<Vec<u8>>> {
    witness_read(private_file::read_private(dir, &name(claim_name), LEN))
}

fn witness_read(read: std::result::Result<Vec<u8>, PrivateFileError>) -> Result<Option<Vec<u8>>> {
    match read {
        Ok(bytes) => Ok(Some(bytes)),
        Err(PrivateFileError::Missing) => Ok(None),
        Err(_) => Err(Error::new("table volume witness is unreadable")),
    }
}

#[cfg(not(test))]
pub(super) fn read(dir: &Path) -> Result<Identity> {
    Ok(aterm_uds::directory_identity::read(dir)?)
}

/// The original device/inode encoding remains immutable, including after tail recovery.
pub(super) fn legacy(dir: &Path) -> Result<[u8; 16]> {
    Ok(legacy_of(read(dir)?))
}

fn legacy_of(identity: Identity) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&identity.dev.to_le_bytes());
    bytes[8..].copy_from_slice(&identity.ino.to_le_bytes());
    bytes
}

pub(super) fn check_observed(
    identity: &Identity,
    witness: Option<&[u8]>,
    claim: &[u8],
) -> Result<()> {
    if claim.len() != CLAIM_LEN {
        return Err(Error::new("table directory claim has an invalid length"));
    }
    let stored = &claim[ANCHOR_LEN..WITNESS_AT];
    if stored[8..] != identity.ino.to_le_bytes() {
        return Err(Error::new(
            "table directory has another inode; recovery required",
        ));
    }
    if let Some(witness) = witness {
        if witness.len() != LEN || &witness[..5] != MAGIC || witness[5..37] != anchor(claim) {
            return Err(Error::new(
                "table volume witness has another custody anchor",
            ));
        }
        if identity
            .volume
            .as_ref()
            .is_none_or(|volume| witness[37..] != *volume)
        {
            return Err(Error::new(
                "table directory has another or unavailable persistent volume; recovery required",
            ));
        }
    } else if stored[..8] != identity.dev.to_le_bytes() {
        return Err(Error::new(
            "table device number changed without a persistent volume witness; recovery required",
        ));
    }
    Ok(())
}

pub(super) fn check(dir: &Path, claim_dir: &Path, claim_name: &str, claim: &[u8]) -> Result<()> {
    checked(dir, claim_dir, claim_name, claim).map(drop)
}

/// [`check`], and the claim now at `claim_name`, none where it cannot be read: one verdict on
/// `claim_dir` for both files, each still opened and judged on its descriptor.
pub(super) fn check_and_read_claim(
    dir: &Path,
    claim_dir: &Path,
    claim_name: &str,
    claim: &[u8],
) -> Result<Option<Vec<u8>>> {
    Ok(checked(dir, claim_dir, claim_name, claim)?
        .read(claim_name, CLAIM_LEN)
        .ok())
}

fn checked<'d>(
    dir: &Path,
    claim_dir: &'d Path,
    claim_name: &str,
    claim: &[u8],
) -> Result<private_file::JudgedDir<'d>> {
    if claim.len() != CLAIM_LEN {
        return Err(Error::new("table directory claim has an invalid length"));
    }
    let identity = read(dir)?;
    let claims = private_file::JudgedDir::judge(claim_dir)
        .map_err(|_| Error::new("table volume witness is unreadable"))?;
    let witness = witness_read(claims.read(&name(claim_name), LEN))?;
    check_observed(&identity, witness.as_deref(), claim)?;
    Ok(claims)
}

/// Called only from a writable open, after the ready claim and complete prefix checked.
pub(super) fn remember(dir: &Path, claim_dir: &Path, claim_name: &str, claim: &[u8]) -> Result<()> {
    if claim.len() != CLAIM_LEN {
        return Err(Error::new("table directory claim has an invalid length"));
    }
    let identity = read(dir)?;
    let existing = witness(claim_dir, claim_name)?;
    check_observed(&identity, existing.as_deref(), claim)?;
    if existing.is_some() {
        return Ok(());
    }
    if claim[ANCHOR_LEN..WITNESS_AT] != legacy_of(identity) {
        return Err(Error::new(
            "table directory changed before its persistent volume was witnessed",
        ));
    }
    let Some(volume) = identity.volume else {
        return Ok(());
    };
    let mut bytes = [0u8; LEN];
    bytes[..5].copy_from_slice(MAGIC);
    bytes[5..37].copy_from_slice(&anchor(claim));
    bytes[37..].copy_from_slice(&volume);
    private_file::check_private_dir(claim_dir)?;
    aterm_secrets::fsx::write_new(&claim_dir.join(name(claim_name)), &bytes, 0o600)
        .map_err(super::super::failure)?;
    aterm_durable::durable_dir(claim_dir)?;
    check(dir, claim_dir, claim_name, claim)
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub(super) struct Probe {
    pub(super) dev_shift: u64,
    pub(super) volume: Option<[u8; 16]>,
}

#[cfg(test)]
std::thread_local! {
    static PROBE: std::cell::Cell<Probe> = const { std::cell::Cell::new(Probe { dev_shift: 0, volume: None }) };
}

#[cfg(test)]
pub(super) fn with_probe<T>(probe: Probe, run: impl FnOnce() -> T) -> T {
    struct Restore(Probe);
    impl Drop for Restore {
        fn drop(&mut self) {
            PROBE.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(PROBE.with(|slot| slot.replace(probe)));
    run()
}

#[cfg(test)]
pub(super) fn read(dir: &Path) -> Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Err(Error::new("table directory is not a directory"));
    }
    let probe = PROBE.with(std::cell::Cell::get);
    Ok(Identity {
        dev: metadata
            .dev()
            .checked_add(probe.dev_shift)
            .ok_or_else(|| Error::new("test device overflow"))?,
        ino: metadata.ino(),
        volume: probe.volume,
    })
}

#[cfg(test)]
mod tests;
