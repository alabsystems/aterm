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
//! What is kept is bounded by its charge, each table's wires' allocations and its records'
//! links, plus [`FIXED`] for the rest: at most [`KEPT_TABLES`] tables, [`JOURNAL_BYTES`] for one
//! and [`KEPT_BYTES`] for all of them. Asks of one table in a row count as one ask, as the opens
//! of one table in a repair round are. Where a table's records do not fit, only tables last asked
//! before its own previous ask (so that it was asked twice since they were) yield it their room,
//! the oldest first; a table asked for the first time takes only free room. So a cycle of more
//! tables than fit keeps those that fit and reads the rest as a fresh read does. A table nobody
//! asks again stays until a table asked twice needs its room, within the bound. Outside the
//! charge are the allocator's rounding, the copies each open's store makes of the records it
//! exposes, and links an open still holds once its table let them go.

use super::{
    group::Boundary,
    replay::{self, Linked, Replay},
};
use crate::{Error, Result};
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
/// A kept table's charge besides its wires and links: its key (folder, anchor and limits), its
/// prefix and its witness, rounded up.
const FIXED: usize = 1024;
/// Of each this many asks in a row that what a table keeps could answer, the last reads the
/// journal again, as an ask after a change does.
const REVALIDATE: u32 = 64;

static JOURNALS: Journals = Journals::new(KEPT_TABLES, KEPT_BYTES, JOURNAL_BYTES);

/// The kept tables, and the bounds on them.
pub(super) struct Journals {
    /// Lookup and the charges alone hold this lock; no journal is read under it.
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
    /// What `table`'s kept records charge; zero while it keeps none.
    charge: usize,
}

struct Table {
    dir: PathBuf,
    anchor: Box<[u8]>,
    limits: [usize; 3],
    /// Held only to look, take or put: never while the journal is read.
    kept: Mutex<Option<Kept>>,
}

struct Kept {
    /// Its acknowledgement alone ([`Retain::Digest`]).
    prefix: Prefix,
    links: Arc<Vec<Linked>>,
    /// The journal as it stood before the read that kept `links`, where it stands for what that
    /// read found ([`JournalSeen::stands_for`]).
    witnessed: Option<JournalSeen>,
    /// Consecutive answers from `links` since they were read.
    hits: u32,
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
    fn is(&self, dir: &Path, anchor: &[u8], limits: [usize; 3]) -> bool {
        self.limits == limits && *self.anchor == *anchor && self.dir == dir
    }
}

impl List {
    /// Let go of what `table`'s kept records charge, where it is still listed. A table that
    /// charges nothing lets go of nothing, so a second release changes nothing.
    fn release(&mut self, table: &Arc<Table>) {
        if let Some(slot) = self
            .slots
            .iter_mut()
            .find(|slot| Arc::ptr_eq(&slot.table, table))
        {
            self.charged -= std::mem::take(&mut slot.charge);
        }
    }

    /// Take the slots `leaves` names off the list, and their charges with them.
    fn delist(&mut self, leaves: impl Fn(&Slot) -> bool) -> Vec<Slot> {
        let (left, stay) = std::mem::take(&mut self.slots)
            .into_iter()
            .partition::<Vec<_>, _>(|slot| leaves(slot));
        self.slots = stay;
        self.charged -= left.iter().map(|slot| slot.charge).sum::<usize>();
        left
    }
}

/// What `links` charge: each wire's allocation, the links' own, and [`FIXED`].
fn charge(links: &Vec<Linked>) -> usize {
    FIXED
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
        // Each step under this lock takes what is kept, puts it back or answers from it, so one
        // that unwound left it whole or gone.
        let known = {
            let mut kept = table.kept.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(kept) = kept.as_mut()
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
            // An ask of this table while this one reads finds nothing kept and reads as well.
            let known = kept.take();
            if known.is_some() {
                self.list
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .release(&table);
            }
            known
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
            known.as_ref().map(|known| &known.prefix),
            Retain::Digest,
        )?;
        #[cfg(test)]
        Self::run(&self.after_read, dir);
        let witnessed = seen.filter(|seen| seen.stands_for(&opened, now));
        let unknown = || Error::new("table journal read extended an unknown prefix");
        let (prefix, links, refused) = match committed {
            Committed::Unchanged => {
                let known = known.ok_or_else(unknown)?;
                (known.prefix, known.links, None)
            }
            Committed::Extended { records, prefix } => {
                let mut links = Arc::unwrap_or_clone(known.ok_or_else(unknown)?.links);
                links.reserve_exact(records.len());
                let refused = replay::identify_onto(&mut links, records).err();
                (prefix, Arc::new(links), refused)
            }
            Committed::Rebuilt { records, prefix } => {
                drop(known);
                let mut links = Vec::with_capacity(records.len());
                let refused = replay::identify_onto(&mut links, records).err();
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
                },
            );
        }
        Ok(Read { links, refused })
    }

    /// The table kept for `dir`, `anchor` and `limits`, listed now if it was not, and the count of
    /// the ask that asked it before this one: asks of one table in a row are one ask, so another
    /// table's ask came between that ask and this. A table listed past [`Self::tables`] lets the
    /// least recently asked one go.
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
                    kept: Mutex::new(None),
                });
                slots.push(Slot {
                    table: Arc::clone(&table),
                    asked: ask,
                    before: None,
                    charge: 0,
                });
                let retired = if list.slots.len() > self.tables {
                    let oldest = list.slots.iter().map(|slot| slot.asked).min();
                    list.delist(|slot| Some(slot.asked) == oldest)
                } else {
                    Vec::new()
                };
                ((table, None), retired)
            }
        };
        // A table let go can own a large history: release it after the lock, as reads run
        // outside it.
        drop(retired);
        found
    }

    /// Keep `kept` for `table` where its charge is admitted ([`Self::admit`]); otherwise keep
    /// nothing for it. Of two reads of one table that overlap, the one put last is kept.
    fn install(&self, table: &Arc<Table>, previous: Option<u64>, kept: Kept) {
        let charge = charge(&kept.links);
        let mut slot = table.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let (admitted, retired) = self.admit(table, previous, charge);
        *slot = admitted.then_some(kept);
        drop(slot);
        drop(retired);
    }

    /// Charge `charge` to `table`'s slot in place of what it charged, where `table` is still
    /// listed, `charge` is within [`Self::journal`], and all charges then fit [`Self::bytes`]:
    /// where they would not, the tables last asked before `table`'s `previous` ask (so that it was
    /// asked twice since they were) yield their room, the oldest first, as many as it needs. A
    /// table asked for the first time takes only free room. On a refusal `table` charges nothing.
    /// Returns whether `charge` was admitted, and the slots let go for it, to be dropped once no
    /// lock is held.
    fn admit(&self, table: &Arc<Table>, previous: Option<u64>, charge: usize) -> (bool, Vec<Slot>) {
        let mut list = self.list.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(own) = list
            .slots
            .iter()
            .find(|slot| Arc::ptr_eq(&slot.table, table))
            .map(|slot| slot.charge)
        else {
            return (false, Vec::new());
        };
        let others = list.charged - own;
        let mut retired = Vec::new();
        if charge > self.journal {
            list.release(table);
            return (false, retired);
        }
        if let Some(need) = (others + charge).checked_sub(self.bytes).filter(|&n| n > 0) {
            let yields = |slot: &Slot| {
                previous.is_some_and(|previous| slot.asked < previous)
                    && slot.charge > 0
                    && !Arc::ptr_eq(&slot.table, table)
            };
            let mut oldest: Vec<(u64, usize)> = list
                .slots
                .iter()
                .filter(|slot| yields(slot))
                .map(|slot| (slot.asked, slot.charge))
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
                list.release(table);
                return (false, retired);
            };
            retired = list.delist(|slot| yields(slot) && slot.asked <= last);
        }
        let List { slots, charged, .. } = &mut *list;
        // Still listed: no slot that yields is `table`'s.
        let Some(slot) = slots
            .iter_mut()
            .find(|slot| Arc::ptr_eq(&slot.table, table))
        else {
            return (false, retired);
        };
        *charged = *charged - slot.charge + charge;
        slot.charge = charge;
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
