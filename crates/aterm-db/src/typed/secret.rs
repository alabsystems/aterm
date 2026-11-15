// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A secret column of a generation this build writes is a `SecretRef` cell that names the
//! `aterm secrets` entry holding the value ([`Store`]): the value is put into the store, and only
//! its name, `{vault, entry}`, goes into the table, its history, its exports and every page that
//! shows it. The verbs that write a secret put it there (`aterm db emails set … --stdin`).
//!
//! The generations builds up to 0.200.0 read hold `sec1:` tokens sealed under the table's
//! audience key instead ([`Sealer`]): a password or TOTP seed there is opened only by a sign-in
//! and shown everywhere as [`MASK`], and a phone number, which is ordinary data, is stored
//! there as a token because those builds read no other form, opened for every reader of the
//! table and sealed again when it is set.

use crate::{Error, Result};
use astream_wire::sync::Digest;
use aterm_json::{Value, json};
use ring::aead;
use serde::{Serialize, Serializer};
use std::{collections::BTreeSet, fmt};
use zeroize::Zeroizing;

/// What every surface shows for a value it must not show: a secret's text, or an earlier build's
/// sealed token met in old history.
pub const MASK: &str = "••••••";
const DOMAIN: &[u8] = b"aterm-db secret cell v1\0";
const MAX_VALUE: usize = 16 * 1024;
/// The prefix of every store entry a table's secret cell names: `alab-<collection>-<column>-<row
/// key>.<tag>` ([`own_entry`], [`fresh_entry`]).
pub const ENTRY_PREFIX: &str = "alab-";

/// Plaintext with no plaintext formatting or serialization. It reaches the secret store
/// ([`Store::put`]) or the one consumer a verb hands it to, never a cell.
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Take ownership of a value supplied by its author.
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub(crate) fn plaintext(&self) -> &str {
        &self.0
    }

    /// Seal under this table's audience key, as the generations builds up to 0.200.0 read
    /// hold a value, retaining the epoch needed to resolve it after a rotation. Coordinates are
    /// authenticated, preventing cell substitution.
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

/// The secret store a table's secret cells name entries of: this Mac's `aterm secrets` vault
/// (`alab::vault::Vault`, `alab::secret::store`), an in-memory one in tests ([`Memory`]).
pub trait Store {
    /// The name of the vault a reference names.
    ///
    /// # Errors
    /// No vault is configured.
    fn vault(&self) -> Result<String>;

    /// Whether the store takes a value now. A store shared with Macs whose build writes each
    /// entry it receives to a plaintext file must refuse while any such Mac reads it. Asked
    /// before a write puts anything, so a refused one writes nothing; every [`Self::put`] holds
    /// to it too.
    ///
    /// # Errors
    /// The store takes no value now, or cannot tell; the error says why, never a value.
    fn admit(&self) -> Result<()>;

    /// Put `value` into `entry`, replacing what it holds, once [`Self::admit`] allows.
    ///
    /// # Errors
    /// The store refuses ([`Self::admit`] included) or cannot be written.
    fn put(&self, entry: &str, value: &Secret) -> Result<()>;

    /// The value `entry` holds here, `None` where it holds none.
    ///
    /// # Errors
    /// The store cannot be read or the entry fails authentication.
    fn get(&self, entry: &str) -> Result<Option<Secret>>;

    /// Remove `entry`; one the store does not hold is removed already.
    ///
    /// # Errors
    /// The store refuses or cannot be written.
    fn remove(&self, entry: &str) -> Result<()>;

    /// Record durably, before a value is put into each of `entries` or the reference naming it
    /// is replaced, that the entry is owed removal, owned by this run (its process and start):
    /// the run removes what it owes itself ([`retire`]), and an entry a run ended without
    /// removing is collected by a [`sweep`], never by another live run.
    ///
    /// # Errors
    /// The record cannot be written.
    fn owe(&self, entries: &[String]) -> Result<()>;

    /// The owed entries ([`Self::owe`]) whose owning run has ended.
    ///
    /// # Errors
    /// The record cannot be read.
    fn orphaned(&self) -> Result<BTreeSet<String>>;

    /// Clear this run's own records of `entries`: each was removed, or a cell names it. A record
    /// another run holds is left to that run, or to a [`sweep`] once it has ended.
    ///
    /// # Errors
    /// The record cannot be written.
    fn paid(&self, entries: &BTreeSet<String>) -> Result<()>;

    /// Clear the records of `entries` whose owning run has ended ([`Self::orphaned`]), for a
    /// [`sweep`]: a record a live run holds now is kept.
    ///
    /// # Errors
    /// The record cannot be written.
    fn cleared(&self, entries: &BTreeSet<String>) -> Result<()>;
}

/// What a [`retire`] or [`sweep`] did: entries removed, and entries still owed because their
/// removal failed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settled {
    pub removed: usize,
    pub left: usize,
}

impl std::ops::Add for Settled {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self {
            removed: self.removed + other.removed,
            left: self.left + other.left,
        }
    }
}

/// Remove exactly `entries`, which this run owes ([`Store::owe`]): the entries its committed
/// write replaced. Each removed is paid; one
/// whose removal fails stays owed, for a [`sweep`] once this run has ended.
///
/// # Errors
/// The owed record cannot be written.
pub fn retire(store: &dyn Store, entries: &BTreeSet<String>) -> Result<Settled> {
    let mut settled = Settled::default();
    let mut paid = BTreeSet::new();
    for entry in entries {
        if store.remove(entry).is_ok() {
            settled.removed += 1;
            paid.insert(entry.clone());
        } else {
            settled.left += 1;
        }
    }
    store.paid(&paid)?;
    Ok(settled)
}

/// Collect what ended runs left owed ([`Store::orphaned`]): remove each such entry no cell names
/// and clear the record of each removed or named. `named` is asked after the owed record is read,
/// so whatever an ended run committed is in it; it answers `None` where the names one view shows
/// are not all the family's (more than one generation on this Mac, [`crate::att::sole_generation`]),
/// and then nothing is collected. An entry a live run owes is never touched.
///
/// # Errors
/// The owed record cannot be read or written, or `named` fails.
pub fn sweep(
    store: &dyn Store,
    named: impl FnOnce() -> Result<Option<BTreeSet<String>>>,
) -> Result<Settled> {
    let mut settled = Settled::default();
    let orphaned = store.orphaned()?;
    if orphaned.is_empty() {
        return Ok(settled);
    }
    let Some(named) = named()? else {
        return Ok(settled);
    };
    let mut cleared = BTreeSet::new();
    for entry in orphaned {
        if named.contains(&entry) {
            cleared.insert(entry);
        } else if store.remove(&entry).is_ok() {
            settled.removed += 1;
            cleared.insert(entry);
        } else {
            settled.left += 1;
        }
    }
    store.cleared(&cleared)?;
    Ok(settled)
}

/// The longest store entry name a cell may name: the vault's own limit on an entry name
/// (`aterm_secrets::text::valid_entry_name`), which a row's own entry and its `.<tag>` suffix
/// fit in for every secret column, where the Commons name limit (`crypto::name`, 64) would leave
/// no room for the tag of `mailbox.mailbox_password`.
pub const ENTRY_MAX: usize = 128;

/// Whether `name` has an entry name's shape: up to [`ENTRY_MAX`] of `[A-Za-z0-9._-]`.
pub fn entry_text(name: &str) -> bool {
    (1..=ENTRY_MAX).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// The store entry the secret cell `collection.column` of row `pk` names:
/// `alab-<collection>-<column>-<pk>`.
///
/// # Errors
/// The coordinates make no entry name ([`entry_text`]).
pub fn entry_name(collection: &str, column: &str, pk: &str) -> Result<String> {
    let name = format!("{ENTRY_PREFIX}{collection}-{column}-{pk}");
    if entry_text(&name) && super::pk_grammar(pk) && super::column_name(column) {
        Ok(name)
    } else {
        Err(Error::new(
            "these coordinates make no secret-store entry name",
        ))
    }
}

/// Whether `name` is a store entry name [`entry_name`], [`fresh_entry`] or [`moved_entry`] makes
/// (`alab-<collection>-<column>-<pk>`, perhaps with a `.<tag>` suffix): a name, never a value, so
/// the free-text credential guard, which a long key-shaped run trips, does not judge it.
pub fn is_entry_name(name: &str) -> bool {
    let name = tagged(name).map_or(name, |(own, _)| own);
    let Some(rest) = name.strip_prefix(ENTRY_PREFIX) else {
        return false;
    };
    let Some((collection, rest)) = rest.split_once('-') else {
        return false;
    };
    let Some((column, pk)) = rest.split_once('-') else {
        return false;
    };
    entry_text(name)
        && super::column_name(collection)
        && super::column_name(column)
        && super::pk_grammar(pk)
}

/// A `SecretRef` cell's value: the names of `entry` in `vault`, never its value.
pub fn reference(vault: &str, entry: &str) -> Value {
    json!({"vault": vault, "entry": entry})
}

/// The store entry a `SecretRef` value names.
pub fn entry_of(value: &Value) -> Option<&str> {
    value.get("entry").and_then(Value::as_str)
}

/// The length of an entry's tag, in lowercase hex digits ([`tagged_entry`]).
pub const TAG_LEN: usize = 16;

/// Whether `entry` is an entry the secret cell `collection.column` of row `pk` may name: its own
/// ([`entry_name`]) or that name with a `.<tag>` suffix ([`tagged_entry`]), which each value a
/// write or a roll puts takes. A row key never holds a dot, so no tagged name is another row's
/// own entry. A reference to any other entry is refused when written and never read, so a cell
/// can never reach an entry it does not own.
pub fn bound(collection: &str, column: &str, pk: &str, entry: &str) -> bool {
    let Ok(own) = entry_name(collection, column, pk) else {
        return false;
    };
    entry == own || tagged(entry).is_some_and(|(stem, _)| stem == own) && entry_text(entry)
}

/// `entry` split at a `.<tag>` suffix, the tag [`TAG_LEN`] lowercase hex digits.
fn tagged(entry: &str) -> Option<(&str, &str)> {
    let (stem, tag) = entry.rsplit_once('.')?;
    (tag.len() == TAG_LEN
        && tag
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some((stem, tag))
}

/// The entry of the cell `collection.column` of row `pk` tagged `tag`: its own entry, then `.`
/// and the tag in lowercase hex.
///
/// # Errors
/// The coordinates make no entry name.
pub fn tagged_entry(
    (collection, column, pk): (&str, &str, &str),
    tag: [u8; TAG_LEN / 2],
) -> Result<String> {
    let name = format!(
        "{}.{}",
        entry_name(collection, column, pk)?,
        aterm_codec::hex::encode_lower(&tag)
    );
    if entry_text(&name) {
        Ok(name)
    } else {
        Err(Error::new(
            "these coordinates make no secret-store entry name",
        ))
    }
}

/// The entry a roll moves the value of the candidate written by operation `origin` into: tagged
/// with the operation's digest, so every Mac that rolls the same candidate names the same entry
/// and puts the same value into it.
///
/// # Errors
/// The coordinates make no entry name.
pub fn moved_entry(coords: (&str, &str, &str), origin: &[u8; 32]) -> Result<String> {
    let mut tag = [0; TAG_LEN / 2];
    tag.copy_from_slice(&origin[..TAG_LEN / 2]);
    tagged_entry(coords, tag)
}

/// The entry of the cell tagged with `n` in every byte, for tests that need a fixed tagged name.
///
/// # Panics
/// The coordinates make no entry name.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn test_entry(coords: (&str, &str, &str), n: u8) -> String {
    tagged_entry(coords, [n; TAG_LEN / 2]).expect("test coordinates make an entry name")
}

/// A new entry of the cell for one value a write puts: tagged with fresh randomness, so no other
/// run, here or on another Mac, ever takes it, and checked empty in the store. An entry so holds
/// the one value its put gave it until it is removed, and a removal, whichever Mac's, never takes
/// a value another run put.
///
/// # Errors
/// The coordinates make no entry name, the system has no randomness, or the store cannot be read.
pub fn fresh_entry(store: &dyn Store, coords: (&str, &str, &str)) -> Result<String> {
    for _ in 0..4 {
        let entry = tagged_entry(coords, crate::crypto::random()?)?;
        if store.get(&entry)?.is_none() {
            return Ok(entry);
        }
    }
    Err(Error::new("no fresh secret-store entry name"))
}

/// The entry a reference held by `cell` names, once it is shown to be the cell's own: bound to
/// its coordinates ([`bound`]) and in `vault`, the store's vault. What every reader of a value
/// asks first.
///
/// # Errors
/// The value is no reference, names another vault, or names an entry not bound to `cell`; each
/// names the coordinates only.
pub fn bound_entry<'a>(reference: &'a Value, cell: Cell<'_>, vault: &str) -> Result<&'a str> {
    let refuse = |why: &str| {
        Error::new(&format!(
            "{}.{} of row {}: {why}",
            cell.collection, cell.column, cell.row
        ))
    };
    let entry = entry_of(reference).ok_or_else(|| refuse("the cell names no store entry"))?;
    if reference.get("vault").and_then(Value::as_str) != Some(vault) {
        return Err(refuse("the cell names another vault's entry"));
    }
    if !bound(cell.collection, cell.column, cell.row, entry) {
        return Err(refuse("the cell names an entry that is not its own"));
    }
    Ok(entry)
}

/// Put `value` into a fresh entry of `collection.column` of row `pk` ([`fresh_entry`]), owed by
/// this run ([`Store::owe`]: paid once its write lands, left owed for a sweep if it is refused),
/// and return the reference its cell holds: the one way a secret meets a table. The store is
/// asked first whether it takes a value now ([`Store::admit`]), so a refused one owes nothing.
///
/// # Errors
/// The coordinates make no entry name, the value is a has-password flag rather than a value
/// ([`flag_like`]), or the store refuses.
pub fn store_cell(
    store: &dyn Store,
    collection: &str,
    column: &str,
    pk: &str,
    value: &Secret,
) -> Result<Value> {
    if flag_like(value) {
        return Err(Error::new(
            "a secret is a value, never a has-password flag; give the value",
        ));
    }
    store.admit()?;
    let entry = fresh_entry(store, (collection, column, pk))?;
    store.owe(std::slice::from_ref(&entry))?;
    store.put(&entry, value)?;
    Ok(reference(&store.vault()?, &entry))
}

/// An in-memory [`Store`], for tests in this crate and its dependents: it keeps every value ever
/// put, so a test can show none of them reached a table.
#[cfg(any(test, debug_assertions))]
#[derive(Default)]
pub struct Memory {
    entries: std::sync::Mutex<std::collections::BTreeMap<String, Zeroizing<String>>>,
    every: std::sync::Mutex<Vec<Zeroizing<String>>>,
    /// Owed entries, each with whether its owning run is still live: a live one is this run's,
    /// for [`Store::paid`], an ended one a [`sweep`]'s, for [`Store::cleared`].
    owed: std::sync::Mutex<std::collections::BTreeMap<String, bool>>,
    /// Entries whose removal fails, for a test's failed-removal case.
    stuck: std::sync::Mutex<BTreeSet<String>>,
    /// What [`Store::admit`] asks, where a test set one ([`Self::floor`]): none admits.
    floor: std::sync::Mutex<Option<Floor>>,
    /// Whether the owed record fails to read or write ([`Self::break_records`]).
    broken: std::sync::atomic::AtomicBool,
    /// Whether a put is taken but not yet held ([`Self::pend`]).
    pending: std::sync::atomic::AtomicBool,
}

/// What a test has a [`Memory`] store's [`Store::admit`] answer.
#[cfg(any(test, debug_assertions))]
type Floor = Box<dyn Fn() -> Result<()> + Send>;

#[cfg(any(test, debug_assertions))]
impl Memory {
    /// The vault name a [`Memory`] store's references name.
    pub const VAULT: &'static str = "test-vault";

    /// Every value ever put, in order, for a test's negative control.
    pub fn every(&self) -> Vec<String> {
        self.every
            .lock()
            .map(|every| every.iter().map(|v| v.as_str().to_owned()).collect())
            .unwrap_or_default()
    }

    /// The entries it holds, by name.
    pub fn names(&self) -> Vec<String> {
        self.entries
            .lock()
            .map(|entries| entries.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Every owed entry, whatever its owner, for a test's assertion.
    pub fn owed(&self) -> BTreeSet<String> {
        self.owed
            .lock()
            .map(|owed| owed.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// End every run that owes an entry, as a process exit does: what they owe is orphaned.
    pub fn end_runs(&self) {
        if let Ok(mut owed) = self.owed.lock() {
            owed.values_mut().for_each(|live| *live = false);
        }
    }

    /// Make each removal of `entry` fail until [`Self::unstick`], as a busy vault does.
    pub fn stick(&self, entry: &str) {
        if let Ok(mut stuck) = self.stuck.lock() {
            stuck.insert(entry.to_owned());
        }
    }

    /// Let `entry`'s removals succeed again.
    pub fn unstick(&self, entry: &str) {
        if let Ok(mut stuck) = self.stuck.lock() {
            stuck.remove(entry);
        }
    }

    /// Judge every [`Store::admit`], and so every put, by `judge`: a test's store that takes no
    /// value now.
    pub fn floor(&self, judge: impl Fn() -> Result<()> + Send + 'static) {
        if let Ok(mut floor) = self.floor.lock() {
            *floor = Some(Box::new(judge));
        }
    }

    /// Take each later put without holding it yet, as a vault write signed on this Mac and not
    /// yet posted is: [`Store::put`] answers Ok and [`Store::get`] does not read the value.
    pub fn pend(&self) {
        self.pending
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Make every read and write of the owed record fail, as an unwritable record directory does.
    pub fn break_records(&self) {
        self.broken
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn records(&self) -> Result<()> {
        if self.broken.load(std::sync::atomic::Ordering::Relaxed) {
            Err(Error::new(
                "the memory store's owed record cannot be written",
            ))
        } else {
            Ok(())
        }
    }

    /// The value `entry` holds, for a test's assertion.
    pub fn held(&self, entry: &str) -> Option<String> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(entry).map(|v| v.as_str().to_owned()))
    }
}

#[cfg(any(test, debug_assertions))]
impl Store for Memory {
    fn vault(&self) -> Result<String> {
        Ok(Self::VAULT.to_owned())
    }

    fn admit(&self) -> Result<()> {
        match &*self
            .floor
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
        {
            Some(judge) => judge(),
            None => Ok(()),
        }
    }

    fn put(&self, entry: &str, value: &Secret) -> Result<()> {
        self.admit()?;
        self.every
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
            .push(Zeroizing::new(value.plaintext().to_owned()));
        if self.pending.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        self.entries
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
            .insert(
                entry.to_owned(),
                Zeroizing::new(value.plaintext().to_owned()),
            );
        Ok(())
    }

    fn get(&self, entry: &str) -> Result<Option<Secret>> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
            .get(entry)
            .map(|v| Secret::new(v.as_str().to_owned())))
    }

    fn remove(&self, entry: &str) -> Result<()> {
        if self
            .stuck
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
            .contains(entry)
        {
            return Err(Error::new("the memory store is busy"));
        }
        self.entries
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
            .remove(entry);
        Ok(())
    }

    fn owe(&self, entries: &[String]) -> Result<()> {
        self.owed
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
            .extend(entries.iter().map(|entry| (entry.clone(), true)));
        Ok(())
    }

    fn orphaned(&self) -> Result<BTreeSet<String>> {
        self.records()?;
        Ok(self
            .owed
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
            .iter()
            .filter(|(_, live)| !**live)
            .map(|(entry, _)| entry.clone())
            .collect())
    }

    fn paid(&self, entries: &BTreeSet<String>) -> Result<()> {
        self.records()?;
        self.owed
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
            .retain(|entry, live| !(*live && entries.contains(entry)));
        Ok(())
    }

    fn cleared(&self, entries: &BTreeSet<String>) -> Result<()> {
        self.records()?;
        self.owed
            .lock()
            .map_err(|_| Error::new("memory store poisoned"))?
            .retain(|entry, live| *live || !entries.contains(entry));
        Ok(())
    }
}

/// One table's sealed cells in a generation builds up to 0.200.0 read: sealing a value at its
/// coordinates and opening a stored token, with the table's audience key resolved inside the
/// pinned table session and never handed out (`att::Handle::view_sealed`,
/// `att::Handle::commit_sealed_until`). A roll between two such generations reseals with it, a
/// sign-in opens a credential with it, and a phone number is opened and sealed with it.
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
#[cfg(unix)]
pub(crate) struct Keyed<'a, F> {
    pub(crate) table: &'a str,
    pub(crate) epoch: Digest,
    pub(crate) key: zeroize::Zeroizing<[u8; 32]>,
    /// The audience key of a stored token's epoch.
    pub(crate) resolve: F,
}

#[cfg(unix)]
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
        let (token_epoch, wire) = parts(token)?;
        let cell = Cell {
            table: self.table,
            collection,
            row,
            column,
        };
        if token_epoch == self.epoch {
            return open_parts(&token_epoch, wire, &self.key, cell);
        }
        let key = (self.resolve)(&token_epoch)?;
        open_parts(&token_epoch, wire, &key, cell)
    }
}

/// A fixed-key sealer for plans over hand-built histories: one table, one epoch. The tests that
/// build one are the unix ones (the registry's store is unix-only).
#[cfg(all(test, unix))]
pub(crate) struct Fixed(pub(crate) &'static str);

#[cfg(all(test, unix))]
impl Fixed {
    /// [`Sealer::seal`] for a test that seals a fixture's value, which never fails.
    pub(crate) fn sealed(
        &self,
        collection: &str,
        row: &str,
        column: &str,
        value: &Secret,
    ) -> String {
        self.seal(collection, row, column, value)
            .expect("a fixed key seals")
    }
}

#[cfg(all(test, unix))]
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

/// The additional data a sealed token's AEAD binds: the epoch and the cell's coordinates.
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

/// Whether a [`Secret`] reads as a has-password flag rather than a value: `1`, `0`, `yes`,
/// `true`, `y`, `n` and their kin, or anything shorter than six characters. Never says which. No
/// such value reaches the store ([`store_cell`]): the import and `set` refuse it.
pub fn flag_like(value: &Secret) -> bool {
    flag_like_str(value.plaintext())
}

pub(crate) fn flag_like_str(raw: &str) -> bool {
    let text = raw.trim();
    text.chars().count() < 6
        || ["true", "false", "yes", "no", "set", "none", "null"]
            .iter()
            .any(|word| text.eq_ignore_ascii_case(word))
}

/// What a flag-shaped [`Secret`] says, where it is a flag word: `Some(true)` for a
/// has-password word (`1`, `yes`, `y`, `true`, `set`, `x`), `Some(false)` for a none word (`0`,
/// `no`, `n`, `false`, "none", `null`, `-`), and `None` for anything else, a short password
/// included. Never says which word it read.
pub fn flag_says(value: &Secret) -> Option<bool> {
    flag_says_str(value.plaintext())
}

pub(crate) fn flag_says_str(raw: &str) -> Option<bool> {
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

/// Whether a password cell's text claims a password: it is neither blank nor a none word. A
/// has-password word and a password both do.
pub fn cell_claims(cell: &str) -> bool {
    !cell.trim().is_empty() && flag_says_str(cell) != Some(false)
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

/// Whether a stored value is a bounded version-1 sealed token, without opening it: what a
/// generation builds up to 0.200.0 read holds, and what a surface shows only as [`MASK`].
pub fn is_sealed(token: &str) -> bool {
    parts(token).is_ok()
}

// Only a unix build opens a sealed cell (the store's session is unix-only): elsewhere the
// opener is the tests'.
#[cfg(any(unix, test))]
pub(crate) fn open(token: &str, key: &[u8; 32], cell: Cell<'_>) -> Result<Secret> {
    let (epoch, wire) = parts(token)?;
    open_parts(&epoch, wire, key, cell)
}

#[cfg(any(unix, test))]
fn open_parts(epoch: &Digest, wire: Vec<u8>, key: &[u8; 32], cell: Cell<'_>) -> Result<Secret> {
    let mut wire = Zeroizing::new(wire);
    let nonce: [u8; 12] = wire[..12]
        .try_into()
        .map_err(|_| Error::new("invalid secret nonce"))?;
    let plain_len = cipher(key)?
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(cell.aad(epoch)),
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
#[path = "secret_tests.rs"]
mod tests;
