// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The payloads of a table's records at or below its floor (docs/DESIGN-table-checkpoint-2026-11-15.md
//! §5, tracker LT-B10C3), kept beside the store so a read takes them without opening each record.
//!
//! A read authenticates every record it returns: its signature, its writer's historical grant,
//! the current key and its payload's opening under that key. The first three are asked on every
//! read, a held signature through the fact its wire keeps. The opening is the cost that grows
//! with history, and below a final checkpoint it gives the same plaintext for ever. So when the
//! floor moves ([`super::Store::set_floor`]), the store opens each record it newly covers once and
//! keeps its plaintext here, and a read ([`super::Store::metadata_evidence`]) takes a covered
//! record's payload from this file in place of opening it. What every reader receives is the
//! record it received before, so a read from here equals the full read by construction.
//!
//! The file is sealed to this Mac and this custody: ChaCha20-Poly1305 under a key derived from the
//! table writer's device seed and the table's scope ([`crate::table::Session::cold_key`]), with
//! the custody claim's identity in its authenticated header, as the pending writes are
//! ([`super::pending`]). Each entry names its record by the content identifier of the wire it was
//! opened from, so an entry can stand for no other record. A file that does not open is refused:
//! it is this Mac's own, and one that does not open was changed under it.

use super::{CLAIM_LEN, WITNESS_AT};
use crate::{Error, Result, crypto};
use astream_wire::sync::Digest;
use ring::aead;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The file's name in a store's data folder.
const FILE: &str = "cold";
const MAGIC: &[u8; 5] = b"ATCP1";
const DOMAIN: &[u8] = b"aterm/commons/table-cold-payloads/1\0";
/// The head: [`MAGIC`], the custody binding and the salt.
const HEAD: usize = MAGIC.len() + 32 + 32;
/// What each entry adds to its payload: the record's identifier and the payload's length.
const ENTRY: usize = 32 + 4;

/// The payloads kept, by record identifier.
pub(super) type Payloads = BTreeMap<Digest, Zeroizing<Vec<u8>>>;

fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

fn binding(claim: &[u8]) -> Result<[u8; 32]> {
    if claim.len() != CLAIM_LEN || claim[4] != b'1' {
        return Err(Error::new(
            "cold table payloads require ready table custody",
        ));
    }
    let mut hash = aterm_digest::Sha256::new();
    hash.update(DOMAIN);
    hash.update(&claim[..WITNESS_AT]);
    Ok(hash.finalize())
}

/// The largest file a store whose journal holds at most `journal_bytes` keeps: every held byte
/// as payload, an entry head for each smallest frame, its head and its tag.
fn bound(journal_bytes: usize) -> usize {
    journal_bytes
        .saturating_mul(2)
        .saturating_add(HEAD + 8 + 16)
}

/// The payloads `dir` keeps for the custody `claim`, opened under `key`; none without a file.
///
/// # Errors
/// The file is unreadable, of another custody or Mac, or damaged.
pub(super) fn read(
    dir: &Path,
    claim: &[u8],
    key: &[u8; 32],
    journal_bytes: usize,
) -> Result<Payloads> {
    let Some(bytes) = aterm_secrets::fsx::read_owned(&path(dir), bound(journal_bytes))? else {
        return Ok(Payloads::new());
    };
    let refused = || Error::new("cold table payloads do not open for this custody on this Mac");
    if bytes.len() < HEAD + 8 + 16
        || &bytes[..MAGIC.len()] != MAGIC
        || bytes[MAGIC.len()..MAGIC.len() + 32] != binding(claim)?
    {
        return Err(refused());
    }
    let salt: [u8; 32] = bytes[MAGIC.len() + 32..HEAD].try_into().unwrap();
    let (header, sealed) = bytes.split_at(HEAD);
    let mut sealed = Zeroizing::new(sealed.to_vec());
    let plain = crypto::cipher(key, &salt, header)?
        .open_in_place(
            aead::Nonce::assume_unique_for_key([0; 12]),
            aead::Aad::from(header),
            &mut sealed,
        )
        .map_err(|_| refused())?;
    decode(plain)
}

fn decode(mut plain: &[u8]) -> Result<Payloads> {
    let damaged = || Error::new("cold table payloads are damaged");
    let mut take = |n: usize| -> Result<&[u8]> {
        let (head, rest) = plain.split_at_checked(n).ok_or_else(damaged)?;
        plain = rest;
        Ok(head)
    };
    let count = u64::from_le_bytes(take(8)?.try_into().unwrap());
    let mut payloads = Payloads::new();
    for _ in 0..count {
        let id: Digest = take(32)?.try_into().unwrap();
        let len = u32::from_le_bytes(take(4)?.try_into().unwrap()) as usize;
        let payload = Zeroizing::new(take(len)?.to_vec());
        if payloads.insert(id, payload).is_some() {
            return Err(damaged());
        }
    }
    if !plain.is_empty() {
        return Err(damaged());
    }
    Ok(payloads)
}

/// Keep `payloads` as the cold payloads of `dir` for the custody `claim`, sealed under `key`.
///
/// # Errors
/// The claim is not ready custody, a payload passes a frame's bound, or the file cannot be
/// written.
pub(super) fn write(dir: &Path, claim: &[u8], key: &[u8; 32], payloads: &Payloads) -> Result<()> {
    let plain_len = 8 + payloads
        .values()
        .map(|payload| ENTRY + payload.len())
        .sum::<usize>();
    let salt = crypto::random::<32>()?;
    let mut file = Zeroizing::new(Vec::with_capacity(HEAD + plain_len + 16));
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&binding(claim)?);
    file.extend_from_slice(&salt);
    file.extend_from_slice(&(payloads.len() as u64).to_le_bytes());
    for (id, payload) in payloads {
        let len = u32::try_from(payload.len())
            .map_err(|_| Error::new("a cold table payload passes its bound"))?;
        file.extend_from_slice(id);
        file.extend_from_slice(&len.to_le_bytes());
        file.extend_from_slice(payload);
    }
    let (header, body) = file.split_at_mut(HEAD);
    let tag = crypto::cipher(key, &salt, header)?
        .seal_in_place_separate_tag(
            aead::Nonce::assume_unique_for_key([0; 12]),
            aead::Aad::from(&*header),
            body,
        )
        .map_err(|_| Error::new("cold table payloads could not be sealed"))?;
    file.extend_from_slice(tag.as_ref());
    aterm_secrets::fsx::write_atomic(&path(dir), &file, 0o600).map_err(Error::from)
}

#[cfg(test)]
#[path = "cold_tests.rs"]
mod tests;
