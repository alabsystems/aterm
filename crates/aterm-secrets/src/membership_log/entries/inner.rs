// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Claude Code token class's inner seal (`docs/DESIGN-claude-setup-token-2026-11-05.md` §4.4).
//!
//! In a `Put`'s plaintext, the value of a class name ([`crate::entry_class::claude_token`]) is
//! sealed a second time, under a key derived by HKDF-SHA-256 from the epoch key of the `Put` that
//! seals it, with the domain `aterm/claude-token/1` and the network, the sealing epoch and the
//! entry name as info. The stored value is [`MAGIC`], the sealing epoch's id, a nonce and the
//! ChaCha20-Poly1305 ciphertext, bound to the magic, the epoch and the name. A build that does not
//! know the class decodes only the outer layer, so its materializer, its `secrets get`, its
//! `ls --describe` and an older admin's envelope mirror hold this ciphertext, never the token.
//!
//! In memory, a decoded class value takes one of three forms, which the encoder writes back as
//! they came:
//!
//! - the plaintext, where this Mac opens the sealing epoch: a current build's re-`Put` (a
//!   rotation's, a reseal's) seals it again under its own epoch, so the seal moves forward;
//! - the stored bytes verbatim ([`MAGIC`] first), where it does not (a re-`Put` an older build
//!   signed under a newer epoch carries the seal of an epoch this Mac was never handed): carried
//!   as they are, they read `unreadable here`;
//! - [`UNSEALED`] and the stored bytes, where the stored value carries no inner seal (an older
//!   admin's hand `set`), or opens to bytes that begin with the seal's first byte: carried without
//!   the tag, they read `mismatched (value)`, and no re-`Put` of them ever seals them.
//!
//! [`MAGIC`] and [`UNSEALED`] begin with `0xA7`, which begins no UTF-8 text and no token, so a
//! plaintext never reads as either.

use super::super::RecordId;
use crate::{Error, Result};
use ring::{aead, hkdf};
use zeroize::{Zeroize, Zeroizing};

/// What a stored class value begins with: the inner seal, version 1.
pub(crate) const MAGIC: [u8; 4] = [0xA7, b'C', b'T', b'1'];
/// What a class value begins with in memory where the stored value carries no inner seal.
pub(crate) const UNSEALED: [u8; 4] = [0xA7, b'C', b'T', b'0'];
const DOMAIN: &[u8] = b"aterm/claude-token/1";
const NONCE: usize = 12;
const TAG: usize = 16;
/// The bytes the seal adds to a plaintext: the magic, the epoch, the nonce and the tag.
pub(crate) const OVERHEAD: usize = MAGIC.len() + 32 + NONCE + TAG;

/// A class value as it is in memory ([`of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Form<'a> {
    /// Opened: the plaintext.
    Opened(&'a [u8]),
    /// Sealed under the epoch it names, which this Mac does not open.
    Unreadable(RecordId),
    /// Stored with no inner seal.
    Unsealed,
}

/// The form of the in-memory class value `value`.
pub(crate) fn of(value: &[u8]) -> Form<'_> {
    if value.starts_with(&UNSEALED) {
        return Form::Unsealed;
    }
    match sealing_epoch(value) {
        Some(epoch) => Form::Unreadable(epoch),
        None => Form::Opened(value),
    }
}

/// The epoch a stored (or carried) inner seal names, where `value` is one.
pub(crate) fn sealing_epoch(value: &[u8]) -> Option<RecordId> {
    if !value.starts_with(&MAGIC) || value.len() < OVERHEAD {
        return None;
    }
    let mut epoch = [0u8; 32];
    epoch.copy_from_slice(&value[MAGIC.len()..MAGIC.len() + 32]);
    Some(epoch)
}

fn key(
    epoch_key: &[u8; 32],
    network: &RecordId,
    epoch: &RecordId,
    name: &str,
) -> aead::LessSafeKey {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(epoch_key);
    let info = [
        DOMAIN,
        network.as_slice(),
        epoch.as_slice(),
        name.as_bytes(),
    ];
    let okm = prk
        .expand(&info, &aead::CHACHA20_POLY1305)
        .expect("ChaCha20-Poly1305 key size is within HKDF-SHA-256 bound");
    aead::LessSafeKey::new(aead::UnboundKey::from(okm))
}

fn aad(epoch: &RecordId, name: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(MAGIC.len() + 32 + name.len());
    aad.extend_from_slice(&MAGIC);
    aad.extend_from_slice(epoch);
    aad.extend_from_slice(name.as_bytes());
    aad
}

/// The stored value of the class entry `name` whose in-memory value is `value`, in a `Put` sealed
/// under `epoch` with `epoch_key` on `network`: a plaintext is sealed under that epoch, a carried
/// seal is written verbatim, and an unsealed value without its tag.
///
/// # Errors
/// OS randomness or sealing fails.
pub(crate) fn store(
    network: &RecordId,
    epoch: &RecordId,
    epoch_key: &[u8; 32],
    name: &str,
    value: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let plain = match of(value) {
        Form::Unsealed => return Ok(Zeroizing::new(value[UNSEALED.len()..].to_vec())),
        Form::Unreadable(_) => return Ok(Zeroizing::new(value.to_vec())),
        Form::Opened(plain) => plain,
    };
    let mut nonce = [0u8; NONCE];
    aterm_uds::rand::fill(&mut nonce)?;
    let mut buf = Zeroizing::new(Vec::with_capacity(OVERHEAD + plain.len()));
    buf.extend_from_slice(&MAGIC);
    buf.extend_from_slice(epoch);
    buf.extend_from_slice(&nonce);
    let mut sealed = Vec::with_capacity(plain.len() + TAG);
    sealed.extend_from_slice(plain);
    key(epoch_key, network, epoch, name)
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad(epoch, name)),
            &mut sealed,
        )
        .map_err(|_| {
            sealed.zeroize();
            Error::new("sealing a Claude Code token entry failed")
        })?;
    buf.extend_from_slice(&sealed);
    sealed.zeroize();
    Ok(buf)
}

/// The in-memory value of the class entry `name` whose stored value is `stored`, as a Mac holding
/// the epoch keys `keys` (by epoch id) on `network` reads it ([`Form`]). A seal that names an
/// epoch this Mac holds but does not open under it reads as unsealed: no honest writer makes one.
pub(crate) fn load<'k>(
    network: &RecordId,
    keys: impl Fn(&RecordId) -> Option<&'k Zeroizing<[u8; 32]>>,
    name: &str,
    stored: &[u8],
) -> Zeroizing<Vec<u8>> {
    let unsealed = || {
        let mut out = Zeroizing::new(Vec::with_capacity(UNSEALED.len() + stored.len()));
        out.extend_from_slice(&UNSEALED);
        out.extend_from_slice(stored);
        out
    };
    let Some(epoch) = sealing_epoch(stored) else {
        return unsealed();
    };
    let Some(epoch_key) = keys(&epoch) else {
        return Zeroizing::new(stored.to_vec());
    };
    let header = MAGIC.len() + 32;
    let mut nonce = [0u8; NONCE];
    nonce.copy_from_slice(&stored[header..header + NONCE]);
    let mut buf = Zeroizing::new(stored[header + NONCE..].to_vec());
    let opened = key(epoch_key, network, &epoch, name)
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad(&epoch, name)),
            &mut buf,
        )
        .map(|plain| plain.len());
    match opened {
        Ok(len) if !buf[..len].starts_with(&MAGIC[..1]) => {
            buf.truncate(len);
            buf
        }
        _ => unsealed(),
    }
}

/// The length a stored class value takes for the in-memory `value` (an upper bound for a
/// plaintext, sealed under any epoch).
#[cfg(unix)]
pub(crate) fn stored_len(value: &[u8]) -> usize {
    match of(value) {
        Form::Opened(plain) => plain.len() + OVERHEAD,
        Form::Unreadable(_) => value.len(),
        Form::Unsealed => value.len() - UNSEALED.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Epoch keys by id, as a reader holds them.
    type Keys = BTreeMap<RecordId, Zeroizing<[u8; 32]>>;

    const TOKEN: &[u8] = concat!("sk-ant-", "oat01-inner-fixture_value").as_bytes();

    #[test]
    fn a_sealed_value_opens_under_its_epoch_and_names_it_elsewhere() {
        let (network, epoch, other) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let epoch_key = Zeroizing::new([7u8; 32]);
        let stored = store(&network, &epoch, &epoch_key, "n", TOKEN).unwrap();
        assert!(stored.starts_with(&MAGIC));
        assert!(
            !stored.windows(8).any(|w| TOKEN.windows(8).any(|t| t == w)),
            "the stored value holds no run of the token"
        );
        let mut keys = Keys::new();
        keys.insert(epoch, epoch_key.clone());
        assert_eq!(
            load(&network, |e| keys.get(e), "n", &stored).as_slice(),
            TOKEN
        );
        // Another name, network or key opens nothing: it reads unsealed.
        for (network, name) in [(network, "m"), ([9u8; 32], "n")] {
            let loaded = load(&network, |e| keys.get(e), name, &stored);
            assert_eq!(of(&loaded), Form::Unsealed);
        }
        // Without the sealing epoch's key the bytes are carried, and read unreadable.
        let none = Keys::new();
        let carried = load(&network, |e| none.get(e), "n", &stored);
        assert_eq!(of(&carried), Form::Unreadable(epoch));
        assert_eq!(
            store(&network, &other, &epoch_key, "n", &carried)
                .unwrap()
                .as_slice(),
            stored.as_slice(),
            "a carried seal is written verbatim"
        );
        // A raw value is tagged unsealed, and written back without the tag.
        let raw = load(&network, |e| keys.get(e), "n", TOKEN);
        assert_eq!(of(&raw), Form::Unsealed);
        assert_eq!(
            store(&network, &epoch, &epoch_key, "n", &raw)
                .unwrap()
                .as_slice(),
            TOKEN
        );
        #[cfg(unix)]
        assert_eq!(stored_len(TOKEN), stored.len());
    }
}
