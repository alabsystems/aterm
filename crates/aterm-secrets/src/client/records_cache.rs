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

use super::{Paths, fsx};
use crate::age::Identity;
use crate::{
    Error, SecretBytes,
    membership_log::{MAX_RECORDS_STREAM, RecordBudgets, Store, head},
};
use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom},
    sync::Mutex,
};
use zeroize::Zeroizing;

pub(super) use crate::membership_log::head::Publication as HeadPublication;

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

/// The entries ordinary readers and the records writer of this process share.
static CACHE: Cache = Cache::new();

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    static TEST_CACHE: std::cell::RefCell<Option<std::sync::Arc<Cache>>> =
        const { std::cell::RefCell::new(None) };
}

fn with_cache<T>(ask: impl FnOnce(&Cache) -> T) -> T {
    #[cfg(any(test, debug_assertions))]
    if let Some(cache) = TEST_CACHE.with(|slot| slot.borrow().clone()) {
        return ask(&cache);
    }
    ask(&CACHE)
}

#[cfg(any(test, debug_assertions))]
pub(super) fn with_cold_cache<T>(ask: impl FnOnce() -> T) -> T {
    struct Restore(Option<std::sync::Arc<Cache>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_CACHE.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let _restore =
        Restore(TEST_CACHE.with(|slot| slot.replace(Some(std::sync::Arc::new(Cache::new())))));
    ask()
}

/// This Mac's identity, as a read takes it from its caller: asked only where a read of the file
/// looks for the vault's head or writes one, and where the caller holds no identity, `None`.
pub(super) type IdentityOf<'i> = &'i dyn Fn() -> Option<&'i Identity>;

#[derive(Clone, Copy)]
struct Query<'i> {
    identity: IdentityOf<'i>,
    publication: HeadPublication,
}

/// The store of the records file at `paths`, read under `vault` and `store`'s context; `identity`
/// is this Mac's, which the vault's head is sealed under ([`head`]). `publication` controls
/// filesystem cache writes by this query and its later resolves, not cache authentication
/// or the publishing capability retained by the cache's own store.
pub(super) fn read<'i>(
    paths: &Paths,
    vault: String,
    store: Store,
    identity: IdentityOf<'i>,
    publication: HeadPublication,
) -> crate::Result<Store> {
    with_cache(|cache| {
        cache.read_moving(
            paths,
            vault,
            store,
            Query {
                identity,
                publication,
            },
            |_| {},
        )
    })
}

/// How many complete records the records file `stamp` names holds, where it holds exactly those
/// and `store` is them with complete records after ([`Store::extends`]): what a writer may put
/// on the end of that file instead of replacing it. A read of this process recovered an entry
/// from the file under that stamp, the entry's records are all complete, and their bytes sum
/// to the file's length, so every frame of the file is one of them: no torn tail, no record
/// this build refused, none waiting on a parent, no frame twice. `None` for any other file,
/// and for one no read of this process stamped.
pub(super) fn held_on_file(stamp: fsx::FileStamp, store: &Store) -> Option<usize> {
    with_cache(|cache| cache.held_on_file(stamp, store))
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
    /// Its file, entry and context charge ([`MAX_BYTES`]), beside its store's ([`Entry::kept`]).
    fixed: usize,
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

    /// A read as [`read`] makes one, taking this Mac's identity from `paths`.
    #[cfg(test)]
    fn read(&self, paths: &Paths, vault: String, store: Store) -> crate::Result<Store> {
        let identity = std::cell::OnceCell::new();
        let of = || {
            identity
                .get_or_init(|| super::read_identity(paths).ok().flatten())
                .as_ref()
        };
        self.read_moving(
            paths,
            vault,
            store,
            Query {
                identity: &of,
                publication: HeadPublication::Allowed,
            },
            |_| {},
        )
    }

    /// [`Self::read_noting`], started again while the file moves under it ([`MOVED_READS`]);
    /// `after_observation` is told which try it follows, from 0.
    fn read_moving<'i>(
        &self,
        paths: &Paths,
        vault: String,
        store: Store,
        query: Query<'i>,
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
                query,
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
        let identity = std::cell::OnceCell::new();
        let of = || {
            identity
                .get_or_init(|| super::read_identity(paths).ok().flatten())
                .as_ref()
        };
        self.read_noting(
            paths,
            vault,
            store,
            Query {
                identity: &of,
                publication: HeadPublication::Allowed,
            },
            after_observation,
            &mut None,
        )
    }

    /// One read, which leaves in `opened` the data file it opened and the stamp it opened it
    /// under.
    fn read_noting<'i>(
        &self,
        paths: &Paths,
        vault: String,
        store: Store,
        query: Query<'i>,
        after_observation: impl FnOnce(),
        opened: &mut Option<(std::path::PathBuf, fsx::FileStamp)>,
    ) -> crate::Result<Store> {
        let selected = crate::records_storage::selected(&paths.records_file())?;
        let began = fsx::read_began();
        let Some(mut reader) = fsx::open_owned_reader(selected.path())? else {
            selected.confirm()?;
            return Ok(query.publication.snapshot(store));
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
            return Ok(query.publication.snapshot(entry.store.clone()));
        }
        // The vault's head, where one names this file as it is under this Mac's identity and
        // this read's context: the records without the bodies nothing reads, and without
        // verifying again what this Mac verified before it wrote the head.
        let context = head::context(&store, &vault);
        let head_path = crate::records_storage::head_path(&paths.records_file())?;
        let identity = (query.identity)();
        if std::fs::symlink_metadata(&head_path).is_ok()
            && let Some(identity) = identity
            && let Some((read, digest)) = head::open(
                &head_path,
                reader.file(),
                stamp,
                context,
                identity,
                store.clone(),
                query.publication,
            )
        {
            after_observation();
            reader.check_unchanged()?;
            selected.confirm()?;
            self.keep(vault, keys, network, budgets, digest, stamp, None, &read);
            return Ok(query.publication.snapshot(read));
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
            return Ok(query.publication.snapshot(entry.store.clone()));
        }
        #[cfg(test)]
        self.hashes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let digest = match &observed {
            Observed::Small(bytes) => crate::membership_log::head::digest(bytes.as_slice()),
            Observed::Large(digest) => *digest,
        };
        let same = |entry: &Entry| same_context(entry) && entry.digest == digest;
        if let Ok(mut entries) = self.entries.lock()
            && let Some(entry) = entries.iter_mut().find(|entry| same(entry))
        {
            reader.check_unchanged()?;
            selected.confirm()?;
            entry.known_by(stamp);
            return Ok(query.publication.snapshot(entry.store.clone()));
        }
        let (mut store, bytes) = match observed {
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
        // A resolve of what was read by this Mac's identity writes the head, where every record
        // of the file was read.
        if let Some(identity) = identity {
            store.read_from(head::Origin::new(
                &head_path,
                stamp,
                digest,
                context,
                identity.recipient().0,
                began,
            ));
        }
        self.keep(vault, keys, network, budgets, digest, stamp, bytes, &store);
        Ok(query.publication.snapshot(store))
    }

    /// Keep `store`, read from the file `stamp` names whose SHA-256 is `digest` (its bytes too,
    /// for a small file), as the entry of its context, letting the oldest entries go for its
    /// room.
    #[expect(
        clippy::too_many_arguments,
        reason = "an entry's context, the file it was read from, and the store read"
    )]
    fn keep(
        &self,
        vault: String,
        keys: Option<Vec<[u8; 32]>>,
        network: Option<crate::membership_log::RecordId>,
        budgets: RecordBudgets,
        digest: [u8; 32],
        stamp: fsx::FileStamp,
        bytes: Option<SecretBytes>,
        store: &Store,
    ) {
        let same = |entry: &Entry| {
            entry.vault == vault
                && entry.keys == keys
                && entry.network == network
                && entry.budgets == budgets
                && entry.digest == digest
        };
        // The context keys and the cloned store each retain a lineage vector. Even an
        // empty records file must spend entry/context space, or zero-charge entries can
        // accumulate forever without triggering eviction.
        let lineage_bytes = keys.as_ref().map_or(0, |keys| {
            keys.capacity().saturating_mul(2 * size_of::<[u8; 32]>())
        });
        let fixed = bytes
            .as_ref()
            .map_or(0, SecretBytes::len)
            .saturating_add(size_of::<Entry>())
            .saturating_add(vault.capacity())
            .saturating_add(lineage_bytes);
        let kept = Entry::store_charge(store).saturating_add(fixed);
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|entry| !same(entry));
            if kept <= self.budget {
                let mut total: usize = entries.iter().map(Entry::kept).sum();
                while kept > self.budget.saturating_sub(total)
                    && let Some(oldest) = entries.pop_front()
                {
                    total = total.saturating_sub(oldest.kept());
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
                    fixed,
                });
            }
        }
    }
}

impl Entry {
    /// Its record/index, file, entry and context charge ([`MAX_BYTES`]), as it stands: its
    /// store's is taken again at each count, since a shelved record read back since it was kept
    /// is kept with it ([`Entry::store_charge`]).
    fn kept(&self) -> usize {
        Self::store_charge(&self.store).saturating_add(self.fixed)
    }

    /// What `store` is charged: what it keeps in memory ([`Store::footprint`], a shelved
    /// record's bytes read back included), and, for a store read through the vault's head, the
    /// records file it holds open to read shelved records back from ([`Store::shelf_len`]),
    /// which a writer that replaces the file leaves on disk until the store goes. So the cache
    /// keeps no more replaced files than whole stores of them.
    fn store_charge(store: &Store) -> usize {
        store
            .footprint()
            .saturating_add(usize::try_from(store.shelf_len()).unwrap_or(usize::MAX))
    }

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
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
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
        None => Observed::Large(finish(digest)),
    })
}

/// The SHA-256 `digest` took, through ring's, which runs on the CPU's SHA-256 instructions: a
/// long records file is hashed twice on a full read ([`observe`], [`recover_observation`]), and
/// the portable `aterm_digest::Sha256` took about a third of such an open (ATERM-KI-1113-902).
fn finish(digest: ring::digest::Context) -> [u8; 32] {
    let mut out = [0; 32];
    out.copy_from_slice(digest.finish().as_ref());
    out
}

struct Hashed<R> {
    reader: R,
    digest: ring::digest::Context,
}

impl<R> Hashed<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            digest: ring::digest::Context::new(&ring::digest::SHA256),
        }
    }

    fn finish(self) -> [u8; 32] {
        finish(self.digest)
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
