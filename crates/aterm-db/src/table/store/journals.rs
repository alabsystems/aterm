// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Table journals' identified records, kept across one process's observer opens ([`read`]).
//!
//! An observer open of a table needs its journal's checkpoint-attested prefix read, hashed to the
//! checkpoint, decoded, and each record identified ([`replay::identify_onto`]); a quiet Commons
//! repair round opens each table's store twice a side, and each open did all of that again for
//! journals that had not changed for hours. A process now keeps, for each journal (its folder,
//! anchor and limits), what the last read of it identified:
//!
//! - each record's ciphertext wire exactly as on disk, its SHA-256 identifier, its receipt time,
//!   its conflict flag and the custody chain through it ([`replay::Linked`]), for the whole
//!   checkpoint-attested prefix, the sealed append a claim may not yet witness included;
//! - a successful signature check kept with an immutable wire, where a replay checked it, under
//!   its public signer and the fixed signature domain; this grants no historical permission;
//! - successful authentication of a non-cutoff payload under a comparison token for its exact
//!   payload key; replay can use it only after a fresh key read and historical writer grant;
//! - the prefix's acknowledgement, without its bytes ([`aterm_journal::Retain::Digest`]);
//! - the `lstat` of the folder and its files taken before that read ([`JournalSeen`]), where it
//!   stands for that read.
//!
//! Nothing else: no claim, session, key, plaintext, historical source classification or admission
//! verdict. The retained records are a function of the journal's bytes, anchor and limits alone.
//! The additional cryptographic facts do not change those records or their witness boundary.
//! Each open still judges the claim, source classification, historical writer and key under its
//! own session, with reused keys revalidated before return. Only a known non-cutoff payload can
//! avoid another cryptographic opening during replay; cutoff payloads and actual application
//! payload reads still decrypt ([`Read::witnessed`], then `Store::open_claimed`).
//!
//! An ask whose folder and files stand as that witness showed them is answered from what is kept
//! with nothing opened, read or hashed: a fresh read then would open the same files under the
//! same verdict and read the same bytes ([`JournalSeen::stands_for`]). Any other ask reads the
//! journal handed the kept prefix, which hashes the bytes once and decodes and identifies only the
//! records after it ([`aterm_journal::read_committed_opened`]); a refusal, or a refused record,
//! keeps nothing. A fault that moves no stamp (a bit flipped by the disk, an I/O error a read
//! would meet) is seen at the next read: a writer's open, an ask after any change, or the last of
//! each [`REVALIDATE`] asks in a row that what is kept could answer, which reads the journal again.
//!
//! A read judges the journal's folder by its path before it opens the files in it, as a fresh
//! read does: a folder swapped in for that moment and back is judged in the journal's place by
//! either read, and what such a read kept stands until the next read.
//!
//! A table whose bodies do not fit can instead retain content-bound cryptographic facts alone.
//! Every such ask reads and hashes the journal again; compact facts never answer a physical read,
//! attest a witness or supply historical authority. The freshly read bytes must identify the same
//! complete wire before its signer check or non-cutoff key-comparison token is attached.
//!
//! What is retained is bounded by its allocation charge: owned path capacity and anchor bytes,
//! shared table objects and inline cache metadata, the slot array's capacity, and either full
//! wire/link allocations or compact facts' vector capacity and shared allocations. The bounds
//! are [`KEPT_TABLES`] tables, [`JOURNAL_BYTES`] per table and [`KEPT_BYTES`] across all entries.
//! Asks of one table in a row count as one ask, as the opens of one table in a repair round are.
//! Where a table's payload does not fit, only tables last asked before its own previous ask (so
//! it was asked twice since they were) yield room, oldest first; a first ask takes only free room.
//! When all keys fit, a cycle with more payloads than fit retains those admitted and reads the
//! rest freshly. New metadata can retire the least recently asked key to obey both bounds; a key
//! that cannot fit alone is never listed. Outside the entry charge are the cache container and
//! its single synchronization primitive, allocator rounding, each open's copies, and links still
//! held by an open after the cache lets them go.

use super::{
    group::Boundary,
    replay::{self, Linked, Replay},
};
use crate::{Error, Result, table::signatures::Facts};
use aterm_journal::{Committed, Limits, Prefix, Retain};
use aterm_secrets::fsx::JournalSeen;
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::SystemTime,
};

/// The journal's name in a table's folder.
const NAME: &str = "table";
/// Tables one process keeps.
const KEPT_TABLES: usize = 32;
/// What all kept tables may charge together.
const KEPT_BYTES: usize = 16 << 20;
/// What one kept table may charge.
const JOURNAL_BYTES: usize = 8 << 20;
/// Of each this many asks in a row that what a table keeps could answer, the last reads the
/// journal again, as an ask after a change does.
const REVALIDATE: u32 = 64;

static JOURNALS: Journals = Journals::new(KEPT_TABLES, KEPT_BYTES, JOURNAL_BYTES);

/// The kept tables, and the bounds on them.
pub(super) struct Journals {
    /// Lookup, snapshots, publication and charges hold this lock; no journal is read under it.
    list: Mutex<List>,
    tables: usize,
    bytes: usize,
    journal: usize,
    /// Journal reads begun: asks not answered from what a table keeps.
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
    /// Run before each read, once nothing of the table is locked.
    #[cfg(test)]
    before_read: Mutex<Option<Hook>>,
    /// Run after each read that the journal admitted, before it is identified.
    #[cfg(test)]
    after_read: Mutex<Option<Hook>>,
}

/// A law's hook, run with the folder of the journal about to be read, or just read.
#[cfg(test)]
type Hook = Arc<dyn Fn(&Path) + Send + Sync>;

struct List {
    slots: Vec<Slot>,
    /// Asks so far, those of one table in a row counted once: each ask's count orders the slots
    /// by when they were last asked.
    asks: u64,
    /// What the slots' charges sum to.
    charged: usize,
}

struct Slot {
    table: Arc<Table>,
    /// The count of the ask that last asked this table.
    asked: u64,
    /// The count of the ask that asked this table before `asked`, if one did.
    before: Option<u64>,
    /// Optional pure replay data; publication and checkout use the list lock.
    kept: Option<Kept>,
}

struct Table {
    dir: PathBuf,
    anchor: Box<[u8]>,
    limits: [usize; 3],
}

#[derive(Clone)]
struct Kept {
    /// Its acknowledgement alone ([`Retain::Digest`]).
    prefix: Prefix,
    links: Arc<Vec<Linked>>,
    /// The journal as it stood before the read that kept `links`, where it stands for what that
    /// read found ([`JournalSeen::stands_for`]).
    witnessed: Option<JournalSeen>,
    /// Consecutive answers from `links` since they were read.
    hits: u32,
    /// For a journal whose bodies do not fit: content-bound facts alone. Such a
    /// slot cannot answer a journal read or reuse its prefix without reading bytes.
    detached: Option<Facts>,
    /// Heap charge computed before publication, while no cache lock is held.
    heap_bytes: usize,
}

impl Kept {
    const fn charge(&self) -> usize {
        self.heap_bytes
    }

    fn compact(&mut self) {
        let facts = Facts::of(self.links.iter().map(|link| &link.wire));
        self.links = Arc::new(Vec::new());
        self.heap_bytes = charge(&self.links) + facts.allocation_bytes();
        self.detached = Some(facts);
        self.witnessed = None;
    }
}

impl Slot {
    fn charge(&self) -> usize {
        self.table.allocation_bytes() + self.kept.as_ref().map_or(0, Kept::charge)
    }
}

// What is kept crosses threads and borrows nothing: no session, no cell.
const _: () = {
    const fn kept<T: Send + Sync + 'static>() {}
    kept::<Kept>();
};

/// What one read of a table journal gave: its identified records, and the refusal that stopped
/// their identification, if any.
pub(super) struct Read {
    links: Arc<Vec<Linked>>,
    refused: Option<Error>,
}

impl Read {
    /// The witnessed operation IDs, without copying wires or authenticating their envelopes.
    /// Only repair ordering hints may use these unclassified IDs.
    pub(super) fn repair_ids(
        self,
        acknowledged: impl Into<Boundary>,
    ) -> Result<std::collections::BTreeSet<astream_wire::sync::Digest>> {
        replay::repair_ids(&self.links, self.refused, acknowledged)
    }

    /// This read's records witnessed against `acknowledged` ([`replay::witnessed`]). Replay clones
    /// share immutable wires and their signature facts. A store taking a wire's bytes moves them
    /// where nothing keeps them, and copies them where a kept table does.
    pub(super) fn witnessed(self, acknowledged: impl Into<Boundary>) -> Result<Replay> {
        let acknowledged = acknowledged.into();
        match Arc::try_unwrap(self.links) {
            Ok(links) => replay::witnessed(Cow::Owned(links), self.refused, acknowledged),
            Err(links) => replay::witnessed(Cow::Borrowed(&links), self.refused, acknowledged),
        }
    }
}

/// The identified records of the table journal in `dir` under `anchor` and `limits`, as a read
/// of it now gives them: from what this process keeps where the journal stands as it was read,
/// and read otherwise.
///
/// # Errors
/// [`aterm_journal::read_committed`]'s refusals of the journal.
pub(super) fn read(dir: &Path, anchor: &[u8], limits: Limits) -> Result<Read> {
    let now = asked_now();
    #[cfg(test)]
    if let Some(own) = OWN.with(|own| own.borrow().clone()) {
        return own.read_at(dir, anchor, limits, now);
    }
    JOURNALS.read_at(dir, anchor, limits, now)
}

/// The time an ask's witness is judged settled by: the clock, and in a test as much later as
/// this thread's [`as_if_later`] puts it.
fn asked_now() -> SystemTime {
    let now = SystemTime::now();
    #[cfg(test)]
    let now = now + LATER.get();
    now
}

impl Table {
    /// The shared identity object, owned key allocations and reference counts.
    /// Allocator rounding is excluded.
    fn allocation_bytes(&self) -> usize {
        size_of::<Self>() + 2 * size_of::<usize>() + self.dir.capacity() + self.anchor.len()
    }

    fn is(&self, dir: &Path, anchor: &[u8], limits: [usize; 3]) -> bool {
        self.limits == limits && *self.anchor == *anchor && self.dir == dir
    }
}

impl List {
    fn allocation_bytes(&self) -> usize {
        self.charged
            .saturating_add(self.slots.capacity().saturating_mul(size_of::<Slot>()))
    }

    fn take(&mut self, table: &Arc<Table>) -> Option<Kept> {
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| Arc::ptr_eq(&slot.table, table))?;
        let kept = slot.kept.take()?;
        self.charged -= kept.charge();
        Some(kept)
    }

    fn put(&mut self, table: &Arc<Table>, kept: Kept) {
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| Arc::ptr_eq(&slot.table, table))
            .expect("an admitted table is still listed");
        assert!(
            slot.kept.is_none(),
            "publication follows checkout under the same lock"
        );
        self.charged += kept.charge();
        slot.kept = Some(kept);
    }

    /// Retire complete slots without reallocating the array that stays. Their
    /// table allocations are dropped by the caller after releasing the lock.
    fn delist(&mut self, leaves: impl FnMut(&mut Slot) -> bool) -> Vec<Slot> {
        let left: Vec<_> = self.slots.extract_if(.., leaves).collect();
        self.charged -= left.iter().map(|slot| slot.charge()).sum::<usize>();
        left
    }
}

/// The shared vector object, link capacity and complete wire allocations.
fn charge(links: &Vec<Linked>) -> usize {
    size_of::<Vec<Linked>>()
        + 2 * size_of::<usize>()
        + links
            .iter()
            .map(|link| link.wire.allocation_bytes())
            .sum::<usize>()
        + links.capacity() * size_of::<Linked>()
}

impl Journals {
    pub(super) const fn new(tables: usize, bytes: usize, journal: usize) -> Self {
        Self {
            list: Mutex::new(List {
                slots: Vec::new(),
                asks: 0,
                charged: 0,
            }),
            tables,
            bytes,
            journal,
            #[cfg(test)]
            reads: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            before_read: Mutex::new(None),
            #[cfg(test)]
            after_read: Mutex::new(None),
        }
    }

    /// [`read`], asked at `now`.
    fn read_at(&self, dir: &Path, anchor: &[u8], limits: Limits, now: SystemTime) -> Result<Read> {
        let (table, previous) = self.table(dir, anchor, limits);
        // Taken before the journal is read: a change after this moves the next witness, and a
        // file swapped in for the read is another than this one saw ([`JournalSeen::stands_for`]).
        let seen = JournalSeen::of(dir, NAME);
        // Snapshot and publication share the metadata lock. A physical reread
        // leaves immutable facts available to overlapping readers; no read or
        // cryptographic work runs under this lock.
        let known = {
            let mut list = self.list.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(kept) = list
                .slots
                .iter_mut()
                .find(|slot| Arc::ptr_eq(&slot.table, &table))
                .and_then(|slot| slot.kept.as_mut())
                && kept.detached.is_none()
                && seen.is_some()
                && kept.witnessed == seen
                && kept.hits + 1 < REVALIDATE
            {
                kept.hits += 1;
                return Ok(Read {
                    links: Arc::clone(&kept.links),
                    refused: None,
                });
            }
            list.slots
                .iter()
                .find(|slot| Arc::ptr_eq(&slot.table, &table))
                .and_then(|slot| slot.kept.as_ref())
                .cloned()
        };
        #[cfg(test)]
        {
            Self::run(&self.before_read, dir);
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let (committed, opened) = aterm_journal::read_committed_opened(
            dir,
            NAME,
            anchor,
            limits,
            known
                .as_ref()
                .filter(|known| known.detached.is_none())
                .map(|known| &known.prefix),
            Retain::Digest,
        )?;
        #[cfg(test)]
        Self::run(&self.after_read, dir);
        let witnessed = seen.filter(|seen| seen.stands_for(&opened, now));
        let unknown = || Error::new("table journal read extended an unknown prefix");
        let (prefix, links, refused) = match committed {
            Committed::Unchanged => {
                let known = known.as_ref().ok_or_else(unknown)?;
                (known.prefix.clone(), Arc::clone(&known.links), None)
            }
            Committed::Extended { records, prefix } => {
                let mut links = known.as_ref().ok_or_else(unknown)?.links.as_ref().clone();
                links.reserve_exact(records.len());
                let refused = replay::identify_onto(&mut links, records).err();
                (prefix, Arc::new(links), refused)
            }
            Committed::Rebuilt { records, prefix } => {
                let mut links = Vec::with_capacity(records.len());
                let facts = known.as_ref().and_then(|known| known.detached.as_ref());
                let refused = replay::identify_onto_with(&mut links, records, facts).err();
                (prefix, Arc::new(links), refused)
            }
        };
        if refused.is_none() {
            self.install(
                &table,
                previous,
                Kept {
                    prefix,
                    links: Arc::clone(&links),
                    witnessed,
                    hits: 0,
                    detached: None,
                    heap_bytes: charge(&links),
                },
                known,
            );
        }
        Ok(Read { links, refused })
    }

    /// The table kept for `dir`, `anchor` and `limits`, listed now if it was not, and the count of
    /// the ask that asked it before this one: asks of one table in a row are one ask, so another
    /// table's ask came between that ask and this. A table listed past [`Self::tables`] lets the
    /// least recently asked one go. Metadata also obeys the byte bound: an oversized key is
    /// never listed, and metadata pressure retires the least recently asked listed keys.
    fn table(&self, dir: &Path, anchor: &[u8], limits: Limits) -> (Arc<Table>, Option<u64>) {
        let limits = [limits.records, limits.bytes, limits.record_bytes];
        let (found, retired) = {
            let mut list = self.list.lock().unwrap_or_else(PoisonError::into_inner);
            let List { slots, asks, .. } = &mut *list;
            if let Some(slot) = slots
                .iter_mut()
                .find(|slot| slot.table.is(dir, anchor, limits))
            {
                // The last ask was another table's: this one is a new ask of it.
                if slot.asked != *asks {
                    *asks += 1;
                    slot.before = Some(std::mem::replace(&mut slot.asked, *asks));
                }
                ((Arc::clone(&slot.table), slot.before), Vec::new())
            } else {
                *asks += 1;
                let ask = *asks;
                let table = Arc::new(Table {
                    dir: dir.into(),
                    anchor: anchor.into(),
                    limits,
                });
                let base = table.allocation_bytes();
                // An unretainable key must not displace useful existing entries.
                if self.tables == 0
                    || base.saturating_add(size_of::<Slot>()) > self.journal
                    || base.saturating_add(size_of::<Slot>()) > self.bytes
                {
                    return (table, None);
                }
                slots.reserve_exact(1);
                slots.push(Slot {
                    table: Arc::clone(&table),
                    asked: ask,
                    before: None,
                    kept: None,
                });
                list.charged += base;
                let mut retired = Vec::new();
                while list.slots.len() > self.tables || list.allocation_bytes() > self.bytes {
                    if list.slots.len() <= self.tables && list.slots.capacity() > list.slots.len() {
                        list.slots.shrink_to_fit();
                        if list.slots.len() <= self.tables && list.allocation_bytes() <= self.bytes
                        {
                            break;
                        }
                    }
                    let Some(oldest) = list.slots.iter().map(|slot| slot.asked).min() else {
                        list.slots = Vec::new();
                        break;
                    };
                    retired.extend(list.delist(|slot| slot.asked == oldest));
                }
                ((table, None), retired)
            }
        };
        // A table let go can own a large history: release it after the lock, as reads run
        // outside it.
        drop(retired);
        found
    }

    /// Retain complete bodies where admitted, compact facts otherwise, and preserve an older
    /// usable entry if neither optional replacement fits. Each reader snapshots immutable
    /// data; none supplies authority or a witness without fresh admission.
    fn install(
        &self,
        table: &Arc<Table>,
        previous: Option<u64>,
        mut kept: Kept,
        known: Option<Kept>,
    ) {
        let base = table.allocation_bytes();
        if base
            .saturating_add(kept.charge())
            .saturating_add(size_of::<Slot>())
            > self.journal
            || base.saturating_add(kept.charge()) > self.bytes
        {
            kept.compact();
        }
        let mut candidate = Some(kept);
        let mut fallback: Vec<_> = known.into_iter().collect();
        let mut retired = Vec::new();
        let attempt =
            |candidate: &mut Option<Kept>, fallback: &mut Vec<Kept>, retired: &mut Vec<Slot>| {
                let mut list = self.list.lock().unwrap_or_else(PoisonError::into_inner);
                fallback.extend(list.take(table));
                let charge = base.saturating_add(candidate.as_ref().unwrap().charge());
                let (fits, left) = self.admit(&mut list, table, previous, charge);
                retired.extend(left);
                if fits {
                    list.put(table, candidate.take().unwrap());
                }
                fits
            };
        let mut admitted = attempt(&mut candidate, &mut fallback, &mut retired);
        if !admitted && candidate.as_ref().unwrap().detached.is_none() {
            candidate.as_mut().unwrap().compact();
            admitted = attempt(&mut candidate, &mut fallback, &mut retired);
        }
        if !admitted {
            let mut list = self.list.lock().unwrap_or_else(PoisonError::into_inner);
            fallback.extend(list.take(table));
            let restore = fallback.iter().rposition(|old| {
                let (fits, left) =
                    self.admit(&mut list, table, None, base.saturating_add(old.charge()));
                retired.extend(left);
                fits
            });
            if let Some(at) = restore {
                list.put(table, fallback.swap_remove(at));
            }
        }
        // The single lock protects only metadata, checkout and publication.
        // Wire destruction, fact collection and all I/O remain outside it.
        drop(candidate);
        drop(fallback);
        drop(retired);
    }

    /// Judge a replacement and retire enough eligible older slots to make room.
    /// The caller checks out the old payload and publishes a successful replacement
    /// under this same list lock; refusal leaves its immutable key charged.
    ///
    /// `charge` includes the key and payload. The per-table guard adds its inline
    /// slot; the global guard adds the complete slot-array capacity. Only tables
    /// last asked before this table's previous ask yield payload room, oldest first.
    /// A first ask takes only free room. Retired values are dropped after the lock.
    fn admit(
        &self,
        list: &mut List,
        table: &Arc<Table>,
        previous: Option<u64>,
        charge: usize,
    ) -> (bool, Vec<Slot>) {
        let Some(own) = list
            .slots
            .iter()
            .find(|slot| Arc::ptr_eq(&slot.table, table))
            .map(|slot| slot.charge())
        else {
            return (false, Vec::new());
        };
        let others = list.charged - own;
        let mut retired = Vec::new();
        if charge.saturating_add(size_of::<Slot>()) > self.journal {
            return (false, retired);
        }
        if let Some(need) = others
            .saturating_add(charge)
            .saturating_add(list.slots.capacity().saturating_mul(size_of::<Slot>()))
            .checked_sub(self.bytes)
            .filter(|&n| n > 0)
        {
            let yields = |slot: &Slot| {
                previous.is_some_and(|previous| slot.asked < previous)
                    && slot.charge() > slot.table.allocation_bytes()
                    && !Arc::ptr_eq(&slot.table, table)
            };
            let mut oldest: Vec<(u64, usize)> = list
                .slots
                .iter()
                .filter(|slot| yields(slot))
                .map(|slot| (slot.asked, slot.charge()))
                .collect();
            oldest.sort_unstable();
            let mut freed = 0;
            let mut last = None;
            for (asked, charge) in oldest {
                freed += charge;
                if freed >= need {
                    last = Some(asked);
                    break;
                }
            }
            let Some(last) = last else {
                return (false, retired);
            };
            retired = list.delist(|slot| yields(slot) && slot.asked <= last);
        }
        (true, retired)
    }

    /// Journal reads begun so far ([`Self::reads`]), for another module's laws.
    #[cfg(test)]
    pub(super) fn test_reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    fn run(hook: &Mutex<Option<Hook>>, dir: &Path) {
        let hook = hook.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(hook) = hook {
            hook(dir);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The kept tables this thread's observer opens ask in place of the process's ([`with_own`]).
    static OWN: std::cell::RefCell<Option<Arc<Journals>>> = const { std::cell::RefCell::new(None) };
    /// How much later than the clock this thread's asks are ([`as_if_later`]).
    static LATER: std::cell::Cell<std::time::Duration> =
        const { std::cell::Cell::new(std::time::Duration::ZERO) };
}

/// Run `f` with this thread's observer opens asking `journals` in place of the process's tables.
#[cfg(test)]
pub(super) fn with_own<T>(journals: Arc<Journals>, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<Arc<Journals>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            OWN.with(|own| *own.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(OWN.with(|own| own.borrow_mut().replace(journals)));
    f()
}

/// Run `f` with each of this thread's asks ([`read`]) asked `by` later than the clock: a test's
/// journal has stood settled ([`JournalSeen::stands_for`]) without its waiting.
#[cfg(test)]
pub(super) fn as_if_later<T>(by: std::time::Duration, f: impl FnOnce() -> T) -> T {
    let was = LATER.replace(by);
    let out = f();
    LATER.set(was);
    out
}

#[cfg(test)]
mod tests;
