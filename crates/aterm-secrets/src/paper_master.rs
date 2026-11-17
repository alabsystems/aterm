// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RELEASE ROSTER'S PAPER MASTER PHRASE, KEPT IN THE VAULT (the owner, 2026-11-13: "maybe we
//! should just store the offline master phrase ... seems reasonable if in a secret store"): 52
//! Crockford base32 symbols that derive the Ed25519 master `pins::PAPER_MASTER_PUBKEYS` pins. Its
//! entry is [`ENTRY`], held as release material. The vault is its one store: [`read`] takes it
//! where this Mac's applied registry lands the entry, else files `paper-master` with the lab;
//! [`store`] writes a new one through the admin publish `aterm secrets set` makes. Nothing here
//! prints, logs or echoes a symbol of it: a refusal names a position or a count.

use zeroize::Zeroize as _;

/// The vault entry the phrase is ([`crate::entry_class::PAPER_MASTER`]).
pub const ENTRY: &str = crate::entry_class::PAPER_MASTER;

/// Symbols in a phrase.
pub const LEN: usize = 52;

/// The phrase's alphabet: Crockford base32 in lower case, without i, l, o and u.
pub const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Why bytes are not a phrase; neither reason holds a symbol of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseError {
    /// A byte outside the alphabet at this 1-based symbol position.
    BadChar {
        /// The position.
        position: usize,
    },
    /// Not 52 symbols.
    Length {
        /// How many there were.
        got: usize,
    },
}

impl PhraseError {
    /// The refusal in words, without a symbol of the phrase.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::BadChar { position } => format!(
                "symbol {position} of the master phrase is not a phrase character (0-9 and a-z \
                 without i, l, o, u)"
            ),
            Self::Length { got } => {
                format!("the master phrase must be exactly {LEN} symbols; this one has {got}")
            }
        }
    }
}

/// The 52 symbols `input` spells, written into `out`: ASCII whitespace and `-` are ignored, case
/// is folded, and o reads as 0 and i and l as 1. On a refusal `out` is zeroed.
///
/// # Errors
/// The first byte outside the alphabet, or a count other than 52.
pub fn symbols_into(input: &[u8], out: &mut [u8; LEN]) -> Result<(), PhraseError> {
    let mut n = 0usize;
    let mut refused = None;
    for &byte in input {
        if byte.is_ascii_whitespace() || byte == b'-' {
            continue;
        }
        let folded = match byte.to_ascii_lowercase() {
            b'o' => b'0',
            b'i' | b'l' => b'1',
            other => other,
        };
        if !ALPHABET.contains(&folded) {
            refused = Some(PhraseError::BadChar { position: n + 1 });
            break;
        }
        if n < LEN {
            out[n] = folded;
        }
        n += 1;
    }
    let refused = refused.or((n != LEN).then_some(PhraseError::Length { got: n }));
    match refused {
        Some(why) => {
            out.zeroize();
            Err(why)
        }
        None => Ok(()),
    }
}

/// Whether `bytes` are one phrase ([`symbols_into`]).
#[must_use]
pub fn well_formed(bytes: &[u8]) -> bool {
    let mut out = [0u8; LEN];
    let ok = symbols_into(bytes, &mut out).is_ok();
    out.zeroize();
    ok
}

/// THE PHRASE, FROM THE VAULT ([`crate::registry::resolve`]): [`ENTRY`] read in place from this
/// Mac's store, holding one phrase.
///
/// # Errors
/// One line: this Mac holds no usable phrase (none, or bytes that are not one), why, and the
/// `paper-master` need it filed with the lab's Macs (purpose `release.roster`), or why none was
/// filed. The caller stops there and writes nothing.
#[cfg(any(unix, windows))]
pub fn read(paths: &crate::client::Paths) -> Result<crate::SecretBytes, String> {
    crate::registry::resolve(
        paths,
        &crate::need::Want::PaperMaster,
        crate::need::Purpose::ReleaseRoster,
        |name, value| {
            let mut out = [0u8; LEN];
            let spelled = symbols_into(value.as_slice(), &mut out);
            out.zeroize();
            spelled.map(|()| value).map_err(|why| {
                let mut line = format!("the vault entry {name}");
                line.push_str(" is not a phrase: ");
                line.push_str(&why.message());
                line
            })
        },
    )
}

/// The vault's phrase derives a master the caller does not trust (`why`): file `paper-master`
/// again, as a value its consumer refused, and say so.
#[cfg(any(unix, windows))]
#[must_use]
pub fn refused(paths: &crate::client::Paths, why: &str) -> String {
    crate::need::blocked(
        paths,
        crate::need::Want::PaperMaster,
        crate::need::Purpose::ReleaseRoster,
        why,
    )
}

/// WRITE A NEW PHRASE INTO THE VAULT, for every Mac, as `aterm secrets set` publishes an entry:
/// this Mac opens the vault as its admin and publishes the next version.
///
/// # Errors
/// The phrase is not one ([`symbols_into`]); this Mac cannot open the vault as its admin or
/// publish; or the vault already holds a phrase, which a new one would replace for good (the
/// master it derives would sign nothing again).
#[cfg(any(unix, windows))]
pub fn store(paths: &crate::client::Paths, phrase: &[u8]) -> crate::Result<()> {
    if !well_formed(phrase) {
        return Err(crate::Error::new(
            "refusing to store bytes that are not a master phrase",
        ));
    }
    let admin = crate::client::admin_open(paths)?;
    if admin.doc.entries_ref().any(|entry| entry.name == ENTRY) {
        return Err(crate::Error::new(format!(
            "the vault already holds {ENTRY}; a new master would replace it, and the master it \
             derives could sign no roster again"
        )));
    }
    let mut doc = admin.doc.clone();
    doc.set_entry(&crate::text::Audience::Everyone, ENTRY, phrase)
        .map_err(|error| crate::Error::new(error.to_string()))?;
    if let Some(why) = doc.added_refusal(&admin.doc) {
        return Err(crate::Error::new(why));
    }
    crate::client::admin_publish(paths, &admin, &doc)?;
    Ok(())
}

#[cfg(test)]
#[path = "paper_master_tests.rs"]
mod tests;
