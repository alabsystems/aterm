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

/// Retire the supplemental witness before its claim, so a completed retirement
/// never leaves a detached witness that can be mistaken for lost custody.
pub(crate) fn remove_claim_files(dir: &Path, claim_name: &str) -> Result<()> {
    for file in [
        super::group::name(claim_name),
        name(claim_name),
        claim_name.to_owned(),
    ] {
        match std::fs::remove_file(dir.join(file)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
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

/// Why a claim does not hold the data folder found at its table's path ([`judged`]). Three are
/// relocations: the folder is not found as the claim or its volume witness recorded it
/// ([`Self::Inode`], [`Self::Volume`], [`Self::Device`]). A malformed claim ([`Self::Length`])
/// or a malformed or foreign witness ([`Self::Witness`]) is damaged custody, not a moved folder.
#[derive(Clone, Copy)]
enum Lost {
    /// The claim is not a claim's length.
    Length,
    /// The folder has another inode than the claim recorded. This is given before the claim's
    /// volume witness is read, and says nothing of it.
    Inode,
    /// The claim's volume witness is malformed or was written for another claim.
    Witness,
    /// The folder is on another persistent volume than the claim's own witness names, or on one
    /// that cannot be read.
    Volume,
    /// The folder has another device number than the claim recorded, and no volume witness says
    /// which volume it was on.
    Device,
}

impl Lost {
    fn error(self) -> Error {
        Error::new(match self {
            Self::Length => "table directory claim has an invalid length",
            Self::Inode => "table directory has another inode; recovery required",
            Self::Witness => "table volume witness has another custody anchor",
            Self::Volume => {
                "table directory has another or unavailable persistent volume; recovery required"
            }
            Self::Device => {
                "table device number changed without a persistent volume witness; recovery required"
            }
        })
    }
}

pub(super) fn check_observed(
    identity: &Identity,
    witness: Option<&[u8]>,
    claim: &[u8],
) -> Result<()> {
    judged(identity, witness, claim).map_err(Lost::error)
}

/// Whether `witness` is a volume witness written for `claim`, which is a claim's length: it has
/// a witness's length and magic, and the claim's anchor. Which volume it names is not judged.
fn witnesses(witness: &[u8], claim: &[u8]) -> bool {
    witness.len() == LEN && &witness[..5] == MAGIC && witness[5..37] == anchor(claim)
}

/// Whether `claim` holds the data folder found as `identity`, with the claim's volume witness
/// `witness` where it has one, and why not. The first of its verdicts that applies is given:
/// another inode is named before the witness is read.
fn judged(
    identity: &Identity,
    witness: Option<&[u8]>,
    claim: &[u8],
) -> std::result::Result<(), Lost> {
    if claim.len() != CLAIM_LEN {
        return Err(Lost::Length);
    }
    let stored = &claim[ANCHOR_LEN..WITNESS_AT];
    if stored[8..] != identity.ino.to_le_bytes() {
        return Err(Lost::Inode);
    }
    if let Some(witness) = witness {
        if !witnesses(witness, claim) {
            return Err(Lost::Witness);
        }
        if identity
            .volume
            .as_ref()
            .is_none_or(|volume| witness[37..] != *volume)
        {
            return Err(Lost::Volume);
        }
    } else if stored[..8] != identity.dev.to_le_bytes() {
        return Err(Lost::Device);
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
    let (identity, claims, witness) = observed(dir, claim_dir, claim_name)?;
    check_observed(&identity, witness.as_deref(), claim)?;
    Ok(claims)
}

/// What a judgment of a claim's hold on its data folder reads: the identity of the folder at
/// `dir`, one verdict on `claim_dir`, and the claim's volume witness there, none where it has
/// none.
fn observed<'d>(
    dir: &Path,
    claim_dir: &'d Path,
    claim_name: &str,
) -> Result<(Identity, private_file::JudgedDir<'d>, Option<Vec<u8>>)> {
    let identity = read(dir)?;
    let claims = private_file::JudgedDir::judge(claim_dir)
        .map_err(|_| Error::new("table volume witness is unreadable"))?;
    let witness = witness_read(claims.read(&name(claim_name), LEN))?;
    Ok((identity, claims, witness))
}

/// Where a held read found a displaced table ([`displaced`]): the data folder's identity and the
/// claim's volume witness, as read then. It binds nothing and says nothing of where the folder
/// was when its claim was written; the read keeps it only to notice that either moved while it
/// read ([`pinned_and_read_claim`]).
pub(super) struct Pin {
    identity: Identity,
    volume: Option<Vec<u8>>,
}

/// The [`Pin`] of the data folder at `dir` where `claim` no longer holds it because the folder
/// was relocated (another inode, another or unreadable persistent volume, or another device
/// number with no volume witness); none where the claim holds it, as [`check`] judges. It reads
/// what [`check`] reads and writes nothing. A volume witness beside the claim is held to its
/// form whatever the relocation: [`check`] names another inode without reading the witness, and
/// this refuses a malformed or foreign one there too, in the words [`check`] has for it where
/// the inode is the claim's.
///
/// # Errors
/// The claim has an invalid length, the folder's identity or the claim folder cannot be read, or
/// the claim's volume witness is unreadable, malformed or another claim's: damaged custody,
/// which no read answers from.
pub(super) fn displaced(
    dir: &Path,
    claim_dir: &Path,
    claim_name: &str,
    claim: &[u8],
) -> Result<Option<Pin>> {
    let (identity, _, volume) = observed(dir, claim_dir, claim_name)?;
    match judged(&identity, volume.as_deref(), claim) {
        Ok(()) => Ok(None),
        // The witness, where there is one, was judged the claim's own (`Volume`), or there is
        // none (`Device`).
        Err(Lost::Volume | Lost::Device) => Ok(Some(Pin { identity, volume })),
        // Given before the witness was read, and of a claim of a claim's length, whose anchor
        // `witnesses` may read.
        Err(Lost::Inode) => {
            if volume
                .as_deref()
                .is_some_and(|witness| !witnesses(witness, claim))
            {
                return Err(Lost::Witness.error());
            }
            Ok(Some(Pin { identity, volume }))
        }
        Err(lost @ (Lost::Length | Lost::Witness)) => Err(lost.error()),
    }
}

/// [`check_and_read_claim`] for a held read of a displaced table, which has no binding to judge:
/// the claim now at `claim_name`, none where it cannot be read, once the data folder and the
/// claim's volume witness are found as `pin` holds them.
///
/// # Errors
/// The folder's identity, the claim folder or the volume witness cannot be read; or the folder
/// or the witness is not as `pin` holds it, which is `snapshot_changed`: the read observes
/// again.
pub(super) fn pinned_and_read_claim(
    dir: &Path,
    claim_dir: &Path,
    claim_name: &str,
    pin: &Pin,
) -> Result<Option<Vec<u8>>> {
    let (identity, claims, volume) = observed(dir, claim_dir, claim_name)?;
    if identity != pin.identity || volume != pin.volume {
        return Err(Error::snapshot_changed());
    }
    Ok(claims.read(claim_name, CLAIM_LEN).ok())
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
