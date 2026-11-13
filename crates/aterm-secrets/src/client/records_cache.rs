// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recovered membership records under the current vault and admitted key set, kept within a
//! byte budget ([`MAX_BYTES`]).
//!
//! A read whose opened records file shows a [`fsx::FileStamp`] an entry was read under takes
//! that entry without reading the file again: the stamp tells one file's contents from another's,
//! and every writer of this file renames a new one over it or appends to it, which moves its
//! length. An append that lands while a read holds the file open fails that read's check
//! ([`fsx::OwnedReader::check_unchanged`]), and the read starts again on the file as it then is
//! ([`MOVED_READS`]). An entry keeps the stamps of the last
//! [`STAMPS`] files its bytes were read from, so two vault folders whose records files hold the
//! same bytes in one process (two Macs of a test's lab) do not take each other's turn. Any other
//! read streams the file, and a file past [`MAX_INPUT_BYTES`] is hashed whole. On a Mac whose records file had grown past
//! that bound, each Commons check in a repair round re-hashed it: 3.6 MB about 200 times a round,
//! which held an idle 0.154.0 at two to four cores (2026-11-05, m117).

use super::{Paths, fsx, sha256};
use crate::{
    Error, SecretBytes,
    membership_log::{MAX_RECORDS_STREAM, RecordBudgets, Store},
};
use aterm_digest::Sha256;
use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom},
    sync::Mutex,
};
use zeroize::Zeroizing;

const MAX_INPUT_BYTES: usize = 1024 * 1024;
/// The most files an entry is known by ([`Entry::stamps`]). Kept to one, a read of each of two
/// such files took the entry from the other and read its whole file again at every Commons check
/// of the pair's rounds, hashing it past [`MAX_INPUT_BYTES`]: on a 2.4 MB log most of a quiet
/// round's CPU in `aterm-db`'s two-Mac measurement (2026-11-07).
const STAMPS: usize = 4;
/// The entry charges kept together: records and index ([`Store::footprint`]), a small
/// file's bytes, and entry/context storage. The budget is one maximum store plus a small
/// file; smaller entries share that room, oldest let go first. This is a cache charge,
/// not an allocator or process-footprint measurement. Cached evaluations (membership,
/// effective records and quorum weighing) are not counted; they grow with their records.
const MAX_BYTES: usize = Store::MAX_FOOTPRINT + MAX_INPUT_BYTES;

/// How many times a read starts again on a file that moved under it (another stamp at its path
/// than the one the failed read opened): a member's own writer appends the records a pass
/// brought ([`held_on_file`]) under the state lock, which a reader does not take, so a read
/// that brackets one append reads the whole file at its next try. Each try is checked as the
/// first is; a file that moved under every one of them fails the read as it did before.
const MOVED_READS: usize = 3;

/// The entries every reader and the records writer of this process share.
static CACHE: Cache = Cache::new();

pub(super) fn read(paths: &Paths, vault: String, store: Store) -> crate::Result<Store> {
    CACHE.read(paths, vault, store)
}

/// How many complete records the records file `stamp` names holds, where it holds exactly those
/// and `store` is them with complete records after ([`Store::extends`]): what a writer may put
/// on the end of that file instead of replacing it. A read of this process recovered an entry
/// from the file under that stamp, the entry's records are all complete, and their bytes sum
/// to the file's length, so every frame of the file is one of them: no torn tail, no record
/// this build refused, none waiting on a parent, no frame twice. `None` for any other file,
/// and for one no read of this process stamped.
pub(super) fn held_on_file(stamp: fsx::FileStamp, store: &Store) -> Option<usize> {
    CACHE.held_on_file(stamp, store)
}

struct Cache {
    entries: Mutex<VecDeque<Entry>>,
    /// The most the entries' charges sum to ([`MAX_BYTES`]).
    budget: usize,
    #[cfg(test)]
    hashes: std::sync::atomic::AtomicUsize,
    /// How many reads streamed the records file ([`observe`]).
    #[cfg(test)]
    observations: std::sync::atomic::AtomicUsize,
}

struct Entry {
    vault: String,
    keys: Option<Vec<[u8; 32]>>,
    network: Option<crate::membership_log::RecordId>,
    /// The budgets its store holds records within: the same bytes recover to another store under
    /// others.
    budgets: RecordBudgets,
    digest: [u8; 32],
    /// The stamps of up to [`STAMPS`] opened files these bytes were read from, each unchanged
    /// across its read, the one known longest first: in the entry itself, so its charge
    /// ([`Entry::kept`]) holds them.
    stamps: [Option<fsx::FileStamp>; STAMPS],
    bytes: Option<SecretBytes>,
    store: Store,
    /// Its record/index, file, entry and context charge ([`MAX_BYTES`]).
    kept: usize,
}

impl Cache {
    const fn new() -> Self {
        Self::with_budget(MAX_BYTES)
    }

    const fn with_budget(budget: usize) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            budget,
            #[cfg(test)]
            hashes: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            observations: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn held_on_file(&self, stamp: fsx::FileStamp, store: &Store) -> Option<usize> {
        let entries = self.entries.lock().ok()?;
        entries
            .iter()
            .filter(|entry| {
                entry.stamps.contains(&Some(stamp))
                    && u64::try_from(entry.store.encoded_len()).ok() == Some(stamp.len)
            })
            .find_map(|entry| store.extends(&entry.store))
    }

    fn read(&self, paths: &Paths, vault: String, store: Store) -> crate::Result<Store> {
        self.read_moving(paths, vault, store, |_| {})
    }

    /// [`Self::read_noting`], started again while the file moves under it ([`MOVED_READS`]);
    /// `after_observation` is told which try it follows, from 0.
    fn read_moving(
        &self,
        paths: &Paths,
        vault: String,
        store: Store,
        after_observation: impl Fn(usize),
    ) -> crate::Result<Store> {
        // The first try takes the caller's own name and store, as one read does (an entry
        // charges the name's capacity); only a later try takes a copy.
        let again = (vault.clone(), store.clone());
        let mut first = Some((vault, store));
        let mut attempt = 0;
        loop {
            let (vault, store) = first.take().unwrap_or_else(|| again.clone());
            let mut opened = None;
            let read = self.read_noting(
                paths,
                vault,
                store,
                || after_observation(attempt),
                &mut opened,
            );
            let moved = read.is_err()
                && opened.is_some_and(|(path, stamp): (std::path::PathBuf, fsx::FileStamp)| {
                    fsx::stamp(&path) != Some(stamp)
                });
            if !moved || attempt == MOVED_READS {
                return read;
            }
            attempt += 1;
        }
    }

    /// One read, checked once: what [`Self::read_moving`] tries.
    #[cfg(test)]
    fn read_then(
        &self,
        paths: &Paths,
        vault: String,
        store: Store,
        after_observation: impl FnOnce(),
    ) -> crate::Result<Store> {
        self.read_noting(paths, vault, store, after_observation, &mut None)
    }

    /// One read, which leaves in `opened` the data file it opened and the stamp it opened it
    /// under.
    fn read_noting(
        &self,
        paths: &Paths,
        vault: String,
        store: Store,
        after_observation: impl FnOnce(),
        opened: &mut Option<(std::path::PathBuf, fsx::FileStamp)>,
    ) -> crate::Result<Store> {
        let selected = crate::records_storage::selected(&paths.records_file())?;
        let Some(mut reader) = fsx::open_owned_reader(selected.path())? else {
            selected.confirm()?;
            return Ok(store);
        };
        let stamp = reader.stamp();
        *opened = Some((selected.path().to_path_buf(), stamp));
        let keys = store.keys().map(<[[u8; 32]]>::to_vec);
        let network = store.pinned_network();
        let budgets = store.budgets();
        let same_context = |entry: &Entry| {
            entry.vault == vault
                && entry.keys == keys
                && entry.network == network
                && entry.budgets == budgets
        };
        // A recovered store roots another genesis under different admitted
        // keys, and keeps other records under other budgets. Reuse only under
        // the current key and budget context, and only for the file as it is
        // now: the stamp its bytes were read under, or the bytes themselves.
        if let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries
                .iter()
                .find(|entry| same_context(entry) && entry.stamps.contains(&Some(stamp)))
        {
            after_observation();
            reader.check_unchanged()?;
            selected.confirm()?;
            return Ok(entry.store.clone());
        }
        #[cfg(test)]
        self.observations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let observed = observe(&mut reader)?;
        reader.check_unchanged()?;
        after_observation();
        if let Observed::Small(bytes) = &observed
            && let Ok(mut entries) = self.entries.lock()
            && let Some(entry) = entries.iter_mut().find(|entry| {
                same_context(entry)
                    && entry
                        .bytes
                        .as_ref()
                        .is_some_and(|saved| saved.as_slice() == bytes.as_slice())
            })
        {
            reader.check_unchanged()?;
            selected.confirm()?;
            entry.known_by(stamp);
            return Ok(entry.store.clone());
        }
        #[cfg(test)]
        self.hashes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let digest = match &observed {
            Observed::Small(bytes) => sha256(bytes.as_slice()),
            Observed::Large(digest) => *digest,
        };
        let same = |entry: &Entry| same_context(entry) && entry.digest == digest;
        if let Ok(mut entries) = self.entries.lock()
            && let Some(entry) = entries.iter_mut().find(|entry| same(entry))
        {
            reader.check_unchanged()?;
            selected.confirm()?;
            entry.known_by(stamp);
            return Ok(entry.store.clone());
        }
        let (store, bytes) = match observed {
            Observed::Small(bytes) => (store.recovering(bytes.as_slice()), Some(bytes)),
            Observed::Large(_) => {
                reader
                    .file()
                    .seek(SeekFrom::Start(0))
                    .map_err(|e| Error::new(format!("membership record seek failed: {e}")))?;
                let recovered = recover_observation(
                    store,
                    reader.file().take(MAX_RECORDS_STREAM as u64),
                    digest,
                )?;
                #[cfg(test)]
                self.hashes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                (recovered, None)
            }
        };
        reader.check_unchanged()?;
        selected.confirm()?;
        // The context keys and the cloned store each retain a lineage vector. Even an
        // empty records file must spend entry/context space, or zero-charge entries can
        // accumulate forever without triggering eviction.
        let lineage_bytes = keys.as_ref().map_or(0, |keys| {
            keys.capacity().saturating_mul(2 * size_of::<[u8; 32]>())
        });
        let kept = store
            .footprint()
            .saturating_add(bytes.as_ref().map_or(0, SecretBytes::len))
            .saturating_add(size_of::<Entry>())
            .saturating_add(vault.capacity())
            .saturating_add(lineage_bytes);
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|entry| !same(entry));
            if kept <= self.budget {
                let mut total: usize = entries.iter().map(|entry| entry.kept).sum();
                while kept > self.budget.saturating_sub(total)
                    && let Some(oldest) = entries.pop_front()
                {
                    total -= oldest.kept;
                }
                entries.push_back(Entry {
                    vault,
                    keys,
                    network,
                    budgets,
                    digest,
                    stamps: Entry::known_only_by(stamp),
                    bytes,
                    store: store.clone(),
                    kept,
                });
            }
        }
        Ok(store)
    }
}

impl Entry {
    /// The stamps of an entry first read from the file `stamp` names.
    fn known_only_by(stamp: fsx::FileStamp) -> [Option<fsx::FileStamp>; STAMPS] {
        let mut stamps = [None; STAMPS];
        stamps[STAMPS - 1] = Some(stamp);
        stamps
    }

    /// Its bytes are also those of the file `stamp` names, which takes the place of the stamp it
    /// has known longest once it knows [`STAMPS`].
    fn known_by(&mut self, stamp: fsx::FileStamp) {
        if !self.stamps.contains(&Some(stamp)) {
            self.stamps.rotate_left(1);
            self.stamps[STAMPS - 1] = Some(stamp);
        }
    }
}

enum Observed {
    Small(SecretBytes),
    Large([u8; 32]),
}

fn recover_observation(store: Store, input: impl Read, expected: [u8; 32]) -> crate::Result<Store> {
    let mut input = Hashed::new(input);
    let recovered = store.recovering_read(&mut input)?;
    if input.finish() != expected {
        return Err(Error::new("membership records changed between reads"));
    }
    Ok(recovered)
}

fn observe(reader: &mut fsx::OwnedReader) -> crate::Result<Observed> {
    let mut small = (reader.len() <= MAX_INPUT_BYTES as u64)
        .then(|| Zeroizing::new(Vec::with_capacity(reader.len() as usize + 1)));
    let mut digest = Sha256::new();
    let mut buffer = Zeroizing::new([0; 16 * 1024]);
    let mut input = reader.file().take(MAX_RECORDS_STREAM as u64);
    loop {
        let n = match input.read(&mut *buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::new(format!("membership record read failed: {e}"))),
        };
        if let Some(bytes) = &mut small {
            if bytes.len() + n <= MAX_INPUT_BYTES {
                bytes.extend_from_slice(&buffer[..n]);
                continue;
            }
            digest.update(bytes.as_slice());
            small = None;
        }
        digest.update(&buffer[..n]);
    }
    Ok(match small {
        Some(mut bytes) => Observed::Small(SecretBytes::new(std::mem::take(&mut *bytes))),
        None => Observed::Large(digest.finalize()),
    })
}

struct Hashed<R> {
    reader: R,
    digest: Sha256,
}

impl<R> Hashed<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            digest: Sha256::new(),
        }
    }

    fn finish(self) -> [u8; 32] {
        self.digest.finalize()
    }
}

impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let n = self.reader.read(bytes)?;
        self.digest.update(&bytes[..n]);
        Ok(n)
    }
}

#[cfg(test)]
#[path = "records_cache_tests.rs"]
mod tests;
