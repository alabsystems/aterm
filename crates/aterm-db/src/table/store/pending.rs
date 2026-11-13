// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Table writes a held `Remove` keeps pending (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.2 M4,
//! §11 B11): while the epoch a writer would seal under still wraps a Mac the log removed, the
//! store keeps the operation's payload here, named for that epoch, instead of sealing it under a
//! key the removed Mac holds; once the removal's rotation arrives, [`super::Store::seal_pending`]
//! seals each under the new epoch, in the order they were written. Nothing here is published: a
//! pending write has no wire until it is sealed, so the egress (B7b) never sees one.
//!
//! The file is sealed to this Mac: ChaCha20-Poly1305 under a key derived from the table writer's
//! device seed and the table's scope, so it opens only on the Mac and in the table that wrote it.
//! It is bounded ([`MAX_PENDING`] writes, each at most a table operation's body), so a long wait
//! cannot fill the disk; a write past the bound is refused, as a full store refuses one.

use crate::{Error, Result, crypto};
use astream_wire::sync::Digest;
use ring::aead;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The file's name in a store's data folder.
const FILE: &str = "pending";
/// Its magic.
const MAGIC: &[u8; 5] = b"ATPW1";
/// The most writes a table keeps pending.
pub(crate) const MAX_PENDING: usize = 256;
/// The largest valid `pending` file on disk: header (`MAGIC` + 32-byte salt), 2-byte count, up to
/// [`MAX_PENDING`] records (`1 + 32 + 4 + MAX_BODY`), and 16-byte AEAD tag.
const MAX_PENDING_BYTES: usize =
    MAGIC.len() + 32 + 2 + MAX_PENDING * (1 + 32 + 4 + crypto::MAX_BODY) + 16;

/// The commit a pending write is sealed by: each keeps its own causal shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Shape {
    /// `Store::commit_catalog`: on the catalog frontier.
    Catalog = 1,
    /// `Store::commit_own`: on this writer's own chain.
    Own = 2,
    /// `Store::commit_liability`: on the retained liability evidence.
    Liability = 3,
}

impl Shape {
    const fn of(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Catalog),
            2 => Some(Self::Own),
            3 => Some(Self::Liability),
            _ => None,
        }
    }
}

/// One write kept pending: how it is sealed, the epoch it waited on, and its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pending {
    pub(crate) shape: Shape,
    /// The basis (the epoch record id) the writer would have sealed under.
    pub(crate) epoch: Digest,
    pub(crate) payload: Zeroizing<Vec<u8>>,
}

pub(crate) fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The writes `dir` keeps pending, opened under `key`; none when there is no file.
///
/// # Errors
/// The file is unreadable or does not open under `key` (another Mac's, or damaged).
pub(crate) fn load(dir: &Path, key: &[u8; 32]) -> Result<Vec<Pending>> {
    let path = path(dir);
    let Some(bytes) = aterm_secrets::fsx::read_owned(&path, MAX_PENDING_BYTES)? else {
        return Ok(Vec::new());
    };
    if bytes.len() < MAGIC.len() + 32 + 16 || &bytes[..MAGIC.len()] != MAGIC {
        return Err(Error::new("pending table writes have another format"));
    }
    let salt: [u8; 32] = bytes[MAGIC.len()..MAGIC.len() + 32].try_into().unwrap();
    let header = &bytes[..MAGIC.len() + 32];
    let mut sealed = Zeroizing::new(bytes[MAGIC.len() + 32..].to_vec());
    let plain = crypto::cipher(key, &salt, header)?
        .open_in_place(
            aead::Nonce::assume_unique_for_key([0; 12]),
            aead::Aad::from(header),
            &mut sealed,
        )
        .map_err(|_| Error::new("pending table writes do not open on this Mac"))?;
    decode(plain)
}

fn decode<'a>(mut plain: &'a [u8]) -> Result<Vec<Pending>> {
    let bad = || Error::new("pending table writes are damaged");
    let take = |n: usize, plain: &mut &'a [u8]| -> Result<&'a [u8]> {
        let (head, rest) = plain.split_at_checked(n).ok_or_else(bad)?;
        *plain = rest;
        Ok(head)
    };
    let count = u16::from_le_bytes(take(2, &mut plain)?.try_into().unwrap()) as usize;
    if count > MAX_PENDING {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let shape = Shape::of(take(1, &mut plain)?[0]).ok_or_else(bad)?;
        let epoch: Digest = take(32, &mut plain)?.try_into().unwrap();
        let len = u32::from_le_bytes(take(4, &mut plain)?.try_into().unwrap()) as usize;
        if len > crypto::MAX_BODY {
            return Err(bad());
        }
        let payload = Zeroizing::new(take(len, &mut plain)?.to_vec());
        out.push(Pending {
            shape,
            epoch,
            payload,
        });
    }
    if !plain.is_empty() {
        return Err(bad());
    }
    Ok(out)
}

/// Keep `writes` as the pending set of `dir`, sealed under `key`; an empty set removes the file.
///
/// # Errors
/// The set passes [`MAX_PENDING`], or the file cannot be written.
pub(crate) fn save(dir: &Path, key: &[u8; 32], writes: &[Pending]) -> Result<()> {
    let path = path(dir);
    if writes.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        };
    }
    if writes.len() > MAX_PENDING {
        return Err(Error::new(
            "table writes pending a removal's rotation are at their bound",
        ));
    }
    let plain_len = 2 + writes
        .iter()
        .map(|write| 1 + 32 + 4 + write.payload.len())
        .sum::<usize>();
    let salt = crypto::random::<32>()?;
    let header_len = MAGIC.len() + 32;
    let mut file = Zeroizing::new(Vec::with_capacity(header_len + plain_len + 16));
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&salt);
    file.extend_from_slice(&(writes.len() as u16).to_le_bytes());
    for write in writes {
        file.push(write.shape as u8);
        file.extend_from_slice(&write.epoch);
        file.extend_from_slice(&(write.payload.len() as u32).to_le_bytes());
        file.extend_from_slice(&write.payload);
    }
    let (header, body) = file.split_at_mut(header_len);
    let tag = crypto::cipher(key, &salt, header)?
        .seal_in_place_separate_tag(
            aead::Nonce::assume_unique_for_key([0; 12]),
            aead::Aad::from(&*header),
            body,
        )
        .map_err(|_| Error::new("pending table writes could not be sealed"))?;
    file.extend_from_slice(tag.as_ref());
    aterm_secrets::fsx::write_atomic(&path, &file, 0o600).map_err(Error::from)
}

#[cfg(test)]
#[path = "pending_tests.rs"]
mod tests;
