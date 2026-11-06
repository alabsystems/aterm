// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Secret values have masked formatting and an authenticated, opaque stored form. The
//! inner seal protects display on older readers; table membership still grants access.

use crate::{Error, Result};
use astream_wire::sync::Digest;
use ring::aead;
use serde::{Serialize, Serializer};
use std::fmt;
use zeroize::Zeroizing;

/// The only ordinary rendering of a populated secret.
pub const MASK: &str = "••••••";
const DOMAIN: &[u8] = b"aterm-db secret cell v1\0";
const MAX_VALUE: usize = 16 * 1024;

/// Plaintext with no plaintext formatting or serialization. Use the journalled reveal
/// family in [`crate::alab::secret`] to hand its contents to a consuming surface.
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Take ownership of a value supplied by its author.
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub(crate) fn plaintext(&self) -> &str {
        &self.0
    }

    /// Seal under this table's audience key, retaining the epoch needed to resolve it
    /// after a rotation. Coordinates are authenticated, preventing cell substitution.
    ///
    /// # Errors
    /// The value exceeds the cell limit, randomness fails, or encryption fails.
    pub fn seal(&self, key: &[u8; 32], epoch: &Digest, cell: Cell<'_>) -> Result<String> {
        if self.0.len() > MAX_VALUE {
            return Err(Error::new("secret cell exceeds its size limit"));
        }
        let nonce = crate::crypto::random::<12>()?;
        let mut wire = Zeroizing::new(Vec::with_capacity(12 + self.0.len() + 16));
        wire.extend_from_slice(&nonce);
        wire.extend_from_slice(self.0.as_bytes());
        let tag = cipher(key)?
            .seal_in_place_separate_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(cell.aad(epoch)),
                &mut wire[12..],
            )
            .map_err(|_| Error::new("secret cell could not be sealed"))?;
        wire.extend_from_slice(tag.as_ref());
        let b64 = aterm_secrets::b64::encode_padded(&wire);
        let mut out = String::with_capacity(5 + 64 + 1 + b64.len());
        out.push_str("sec1:");
        aterm_codec::hex::push_lower(&mut out, epoch);
        out.push(':');
        out.push_str(&b64);
        Ok(out)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}
impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(MASK)
    }
}

impl Secret {
    /// Whether two authored values are the same text. No plaintext leaves.
    pub fn same(&self, other: &Self) -> bool {
        let (a, b) = (self.0.as_bytes(), other.0.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    /// The value's length in bytes, for the importer's flag-like test; never the text.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the value is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One table's Secret cells, as the importer and a roll use them: sealing an authored value at
/// its coordinates and opening a stored token, with the table's audience key resolved inside
/// the pinned table session and never handed out (`att::Handle::view_sealed`,
/// `att::Handle::commit_sealed_until`). Opening here hands no plaintext to a surface, so it writes no
/// reveal record; a surface reveals through [`crate::alab::secret::reveal`].
pub trait Sealer {
    /// The table these coordinates name (`alab/registry`).
    fn table(&self) -> &str;

    /// Seal `value` for `collection.row.column` under the session's current audience epoch.
    ///
    /// # Errors
    /// The table key is unavailable or sealing fails.
    fn seal(&self, collection: &str, row: &str, column: &str, value: &Secret) -> Result<String>;

    /// Open `token`, stored at `collection.row.column`, under the epoch it names.
    ///
    /// # Errors
    /// The epoch's key is unavailable or the token does not authenticate at these coordinates.
    fn open(&self, collection: &str, row: &str, column: &str, token: &str) -> Result<Secret>;

    /// Whether `token` at `collection.row.column` opens to `value`. A token that does not
    /// authenticate there is never equal.
    fn holds(
        &self,
        collection: &str,
        row: &str,
        column: &str,
        token: &str,
        value: &Secret,
    ) -> bool {
        self.open(collection, row, column, token)
            .is_ok_and(|held| held.same(value))
    }
}

/// A [`Sealer`] with an explicit key and epoch: the session's resolver in `att`, and fixed test
/// keys in the importer's plans.
pub(crate) struct Keyed<'a, F> {
    pub(crate) table: &'a str,
    pub(crate) epoch: Digest,
    pub(crate) key: zeroize::Zeroizing<[u8; 32]>,
    /// The audience key of a stored token's epoch.
    pub(crate) resolve: F,
}

impl<F: Fn(&Digest) -> Result<zeroize::Zeroizing<[u8; 32]>>> Sealer for Keyed<'_, F> {
    fn table(&self) -> &str {
        self.table
    }

    fn seal(&self, collection: &str, row: &str, column: &str, value: &Secret) -> Result<String> {
        value.seal(
            &self.key,
            &self.epoch,
            Cell {
                table: self.table,
                collection,
                row,
                column,
            },
        )
    }

    fn open(&self, collection: &str, row: &str, column: &str, token: &str) -> Result<Secret> {
        let key = (self.resolve)(&epoch(token)?)?;
        open(
            token,
            &key,
            Cell {
                table: self.table,
                collection,
                row,
                column,
            },
        )
    }
}

/// A fixed-key sealer for plans over hand-built histories: one table, one epoch.
#[cfg(test)]
pub(crate) struct Fixed(pub(crate) &'static str);

#[cfg(test)]
impl Sealer for Fixed {
    fn table(&self) -> &str {
        self.0
    }

    fn seal(&self, collection: &str, row: &str, column: &str, value: &Secret) -> Result<String> {
        let cell = Cell {
            table: self.0,
            collection,
            row,
            column,
        };
        value.seal(&zeroize::Zeroizing::new([7; 32]), &[9; 32], cell)
    }

    fn open(&self, collection: &str, row: &str, column: &str, token: &str) -> Result<Secret> {
        let cell = Cell {
            table: self.0,
            collection,
            row,
            column,
        };
        open(token, &zeroize::Zeroizing::new([7; 32]), cell)
    }
}

/// A secret's authenticated location. Names are metadata, never cell values.
#[derive(Clone, Copy, Debug)]
pub struct Cell<'a> {
    pub table: &'a str,
    pub collection: &'a str,
    pub row: &'a str,
    pub column: &'a str,
}

impl Cell<'_> {
    fn aad(self, epoch: &Digest) -> Vec<u8> {
        let cap = DOMAIN.len()
            + 32
            + 4 * 8
            + self.table.len()
            + self.collection.len()
            + self.row.len()
            + self.column.len();
        let mut out = Vec::with_capacity(cap);
        out.extend_from_slice(DOMAIN);
        out.extend_from_slice(epoch);
        for name in [self.table, self.collection, self.row, self.column] {
            out.extend_from_slice(&(name.len() as u64).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        out
    }
}

fn cipher(key: &[u8; 32]) -> Result<aead::LessSafeKey> {
    let prk = Zeroizing::new(aterm_digest::hkdf_extract(DOMAIN, key));
    let derived = Zeroizing::new(aterm_digest::hkdf_expand_32(&prk, DOMAIN));
    aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &*derived)
        .map(aead::LessSafeKey::new)
        .map_err(|_| Error::new("secret cell key is invalid"))
}

fn parts(token: &str) -> Result<(Digest, Vec<u8>)> {
    let invalid = || Error::new("invalid sealed secret cell");
    if token.len() > (MAX_VALUE + 28).div_ceil(3) * 4 + 70 {
        return Err(invalid());
    }
    let rest = token.strip_prefix("sec1:").ok_or_else(invalid)?;
    let (epoch, body) = rest.split_once(':').ok_or_else(invalid)?;
    let mut id = [0; 32];
    if !aterm_codec::hex::is_lower(epoch, 64)
        || !aterm_codec::hex::decode_into(epoch.as_bytes(), &mut id)
    {
        return Err(invalid());
    }
    let wire = aterm_secrets::b64::decode_padded(body).ok_or_else(invalid)?;
    if wire.len() < 28 || wire.len() > MAX_VALUE + 28 {
        return Err(invalid());
    }
    Ok((id, wire))
}

/// Whether a `Secret` value reads as a has-password flag rather than a value: `1`, `0`, `yes`,
/// `true`, `y`, `n` and their kin, or anything shorter than six characters. Never says which. A
/// `Secret` column refuses such a value wherever it is written (the import, the grid, `set`).
pub fn flag_like(value: &Secret) -> bool {
    let text = value.plaintext().trim();
    text.chars().count() < 6
        || ["true", "false", "yes", "no", "set", "none", "null"]
            .iter()
            .any(|word| text.eq_ignore_ascii_case(word))
}

/// What a flag-shaped `Secret` value says, where it is a flag word: `Some(true)` for a
/// has-password word (`1`, `yes`, `y`, `true`, `set`, `x`), `Some(false)` for a none word (`0`,
/// `no`, `n`, `false`, "none", `null`, `-`), and `None` for anything else, a short password
/// included. Never says which word it read.
pub fn flag_says(value: &Secret) -> Option<bool> {
    flag_says_str(value.plaintext())
}

fn flag_says_str(raw: &str) -> Option<bool> {
    let text = raw.trim();
    if ["1", "yes", "y", "true", "set", "x"]
        .iter()
        .any(|word| text.eq_ignore_ascii_case(word))
    {
        Some(true)
    } else if ["0", "no", "n", "false", "none", "null", "-"]
        .iter()
        .any(|word| text.eq_ignore_ascii_case(word))
    {
        Some(false)
    } else {
        None
    }
}

/// A password cell as a conflict judges it, so that two spellings of one claim do not disagree: no
/// cell, a blank one and a none word read as null, a has-password word reads as `1`, and anything
/// else, a password included, as itself.
pub fn cell_as_compared(cell: Option<&aterm_json::Value>) -> aterm_json::Value {
    use aterm_json::Value;
    let Some(value) = cell else {
        return Value::Null;
    };
    let Some(text) = value.as_str() else {
        return value.clone();
    };
    if text.trim().is_empty() {
        return Value::Null;
    }
    match flag_says_str(text) {
        Some(true) => Value::from("1"),
        Some(false) => Value::Null,
        None => value.clone(),
    }
}

/// The epoch whose audience key opens this cell.
///
/// # Errors
/// The token is malformed or outside the bounded cell size.
pub fn epoch(token: &str) -> Result<Digest> {
    parts(token).map(|(epoch, _)| epoch)
}

/// Whether a stored cell is a bounded version-1 opaque token, without opening it.
pub fn is_sealed(token: &str) -> bool {
    parts(token).is_ok()
}

/// Mask stored tokens recursively, including a newer reader's unknown columns.
pub(crate) fn masked(value: &aterm_json::Value) -> aterm_json::Value {
    use aterm_json::Value;
    match value {
        Value::String(token) if is_sealed(token) => Value::String(MASK.into()),
        Value::Array(items) => Value::Array(items.iter().map(masked).collect()),
        Value::Object(fields) => {
            Value::Object(fields.iter().map(|(k, v)| (k.clone(), masked(v))).collect())
        }
        _ => value.clone(),
    }
}

/// Mask by the declared type even when a malformed writer supplied plaintext.
pub(crate) fn mask_type(ty: &super::Ty, value: &aterm_json::Value) -> aterm_json::Value {
    use super::Ty;
    use aterm_json::Value;
    match (ty, value) {
        (Ty::Secret, Value::Null) => Value::Null,
        (Ty::Secret, _) => Value::String(MASK.into()),
        (Ty::List(inner, _) | Ty::Set(inner), Value::Array(items)) => {
            Value::Array(items.iter().map(|v| mask_type(inner, v)).collect())
        }
        (Ty::Struct(members), Value::Object(fields)) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| {
                    let safe = members
                        .iter()
                        .find(|m| m.name == k)
                        .map_or_else(|| masked(v), |m| mask_type(&m.ty, v));
                    (k.clone(), safe)
                })
                .collect(),
        ),
        _ => masked(value),
    }
}

pub(crate) fn open(token: &str, key: &[u8; 32], cell: Cell<'_>) -> Result<Secret> {
    let (epoch, wire) = parts(token)?;
    let mut wire = Zeroizing::new(wire);
    let nonce: [u8; 12] = wire[..12]
        .try_into()
        .map_err(|_| Error::new("invalid secret nonce"))?;
    let plain_len = cipher(key)?
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(cell.aad(&epoch)),
            &mut wire[12..],
        )
        .map_err(|_| Error::new("secret cell authentication failed"))?
        .len();
    wire.copy_within(12..12 + plain_len, 0);
    wire[plain_len..].fill(0);
    wire.truncate(plain_len);
    let bytes = std::mem::take(&mut *wire);
    let text = String::from_utf8(bytes).map_err(|err| {
        let _ = Zeroizing::new(err.into_bytes());
        Error::new("secret cell is not UTF-8")
    })?;
    Ok(Secret::new(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    const LOCATION: Cell<'static> = Cell {
        table: "alab/registry",
        collection: "mailbox",
        row: "mb-fixture",
        column: "password",
    };

    #[test]
    fn formats_mask_and_old_readers_get_only_a_seal() {
        let value = "A!b?C#d$E%f^G&h*J(k)";
        let secret = Secret::new(value.into());
        for shown in [
            format!("{secret}"),
            format!("{secret:?}"),
            aterm_json::to_string(&secret).unwrap(),
        ] {
            assert!(!shown.contains(value));
            assert!(shown.contains(MASK));
        }
        let token = secret.seal(&[42; 32], &[7; 32], LOCATION).unwrap();
        assert!(is_sealed(&token));
        for part in value.as_bytes().windows(4) {
            assert!(!token.as_bytes().windows(4).any(|word| word == part));
        }
        assert_eq!(
            open(&token, &[42; 32], LOCATION).unwrap().plaintext(),
            value
        );
        assert!(open(&token, &[41; 32], LOCATION).is_err());
        assert!(
            open(
                &token,
                &[42; 32],
                Cell {
                    row: "mb-other",
                    ..LOCATION
                }
            )
            .is_err()
        );
        assert!(!is_sealed(value)); // Negative control: plaintext cannot enter a Secret cell.
    }

    #[test]
    fn secret_guard_exemption_and_ordinary_notes() {
        use crate::typed::{Ty, cell, guard};
        let value = concat!("gh", "p_syntheticCredentialForSecretTest1234567890");
        let token = Secret::new(value.into())
            .seal(&[42; 32], &[7; 32], LOCATION)
            .unwrap();
        assert!(cell::admissible(
            &Ty::Secret,
            &aterm_json::Value::String(token)
        ));
        assert!(!cell::admissible(
            &Ty::Secret,
            &aterm_json::Value::String(value.into())
        ));
        assert!(guard::text(value, 512).is_err());
        assert!(guard::text("+1 (202) 555-0149", 512).is_err());
        assert!(guard::note_cell("Annual plan renewed 2026-11-01; used for the design team. Check billing next November.", 512).is_ok());
    }

    #[test]
    fn typed_and_raw_outputs_mask_even_when_addresses_are_revealed() {
        use crate::typed::{Ty, grid};
        let token = Secret::new("synthetic-password".into())
            .seal(&[42; 32], &[7; 32], LOCATION)
            .unwrap();
        let value = aterm_json::Value::String(token);
        for show in [false, true] {
            assert_eq!(
                grid::show(
                    &Ty::Secret,
                    &aterm_json::Value::String("short-plain".into()),
                    show
                ),
                MASK
            );
            assert_eq!(grid::show(&Ty::Secret, &value, show), MASK);
            assert_eq!(
                grid::raw_masked(&value, show),
                aterm_json::Value::String(MASK.into())
            );
        }
    }

    #[test]
    fn secret_reads_on_another_mac_and_after_audience_rotation() {
        use crate::table::{Session, tests::Lab};
        use aterm_secrets::commons::roster::Role;
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let scope = session.scope().unwrap();
        let basis = session.secret_basis();
        let token = Secret::new("isolated-fixture".into())
            .seal(
                &session.secret_key(scope, &basis).unwrap(),
                &basis,
                LOCATION,
            )
            .unwrap();
        drop(session);
        drop(bootstrap);
        let read = || {
            let bootstrap = lab.bootstrap(1);
            let session = Session::open(&bootstrap).unwrap();
            let key = session.secret_key(scope, &basis).unwrap();
            assert_eq!(
                open(&token, &key, LOCATION).unwrap().plaintext(),
                "isolated-fixture"
            );
        };
        read();
        lab.rotate(&[Role::Writer, Role::Writer]);
        read();
    }
}
