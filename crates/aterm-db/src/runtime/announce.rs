// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a local append tells the serving owner: the tables this process wrote since the owner
//! last looked, so its next rounds repair them first instead of at each pair's turn.
//!
//! The listener that notes them runs under the control journal lock and the table flock
//! (`crate::table::store::listen`), so it waits for nothing and allocates nothing: atomic stores
//! into a fixed bank, then one write to the owner's doorbell, a nonblocking pipe. Only what this
//! process wrote is noted: a record a repair round ingested is the peer's write, a record the
//! mesh delivered travels by the mesh, and a recovery replica authors nothing. An append by
//! another process is not noted here at all: no listener of this process hears it, and it
//! reaches the peers at each pair's ordinary turn.

use crate::table::store::{Appended, Via};
use astream_wire::sync::Digest;
use aterm_uds::ready::Doorbell;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
};

thread_local! {
    /// This thread runs a repair round ([`on_round`]).
    static ROUND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark the calling thread as one that runs a repair round, for its whole life: what it appends
/// is what its round ingested, which the record's own writer announced, so no listener
/// announces it again. Without this each receiver would dial every peer for each record a round
/// brought it.
pub(super) fn on_round() {
    ROUND.with(|round| round.set(true));
}

/// The tables one turn names apart; an append to any further one asks for every table.
const SLOTS: usize = 8;

const EMPTY: u8 = 0;
/// One thread holds the slot: it alone reads or writes the slot's table.
const HELD: u8 = 1;
const FULL: u8 = 2;

#[derive(Default)]
struct Slot {
    state: AtomicU8,
    table: [AtomicU64; 4],
}

impl Slot {
    fn write(&self, table: &Digest) {
        for (word, bytes) in self.table.iter().zip(table.as_chunks::<8>().0) {
            word.store(u64::from_le_bytes(*bytes), Ordering::Relaxed);
        }
    }
    fn read(&self) -> Digest {
        let mut table = [0; 32];
        for (word, bytes) in self.table.iter().zip(table.as_chunks_mut::<8>().0) {
            *bytes = word.load(Ordering::Relaxed).to_le_bytes();
        }
        table
    }
    fn hold(&self, from: u8) -> bool {
        self.state
            .compare_exchange(from, HELD, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
    fn release(&self, to: u8) {
        self.state.store(to, Ordering::Release);
    }
}

/// The tables local appends named since the owner last took them ([`Self::take`]), filled
/// without a lock or an allocation ([`Self::note`]).
#[derive(Default)]
pub(super) struct Announced {
    slots: [Slot; SLOTS],
    /// An append named a table no slot had room for: the owner asks its peers which differ.
    all: AtomicBool,
}

impl Announced {
    /// Note an append to `table`. A table noted already and not yet taken is noted once; where
    /// every slot names another table, the append is kept as one to an unnamed table.
    ///
    /// A slot another thread holds this instant is passed over, so a table can be noted twice
    /// (the owner takes a set) and nothing waits. The caller rings the owner after this returns,
    /// so whatever this left noted is taken by a turn that begins after it.
    pub(super) fn note(&self, table: &Digest) {
        for slot in &self.slots {
            if slot.hold(FULL) {
                let same = slot.read() == *table;
                slot.release(FULL);
                if same {
                    return;
                }
            }
        }
        for slot in &self.slots {
            if slot.hold(EMPTY) {
                slot.write(table);
                slot.release(FULL);
                return;
            }
        }
        self.all.store(true, Ordering::Release);
    }

    /// Take what was noted since the last call: the tables named, and whether an append went
    /// unnamed. `None` where nothing was noted.
    pub(super) fn take(&self) -> Option<(BTreeSet<Digest>, bool)> {
        let all = self.all.swap(false, Ordering::AcqRel);
        let mut tables = BTreeSet::new();
        for slot in &self.slots {
            if slot.hold(FULL) {
                tables.insert(slot.read());
                slot.release(EMPTY);
            }
        }
        (all || !tables.is_empty()).then_some((tables, all))
    }

    /// The listener for this bank's custody (`crate::table::store::listen`): it notes each
    /// append this process authored and rings `bell`, the owner's doorbell. It passes over a
    /// recovery replica's appends, what the mesh delivered ([`Via::Mesh`]) and what a repair
    /// round's thread appended ([`on_round`]).
    pub(super) fn listener(
        self: &Arc<Self>,
        bell: Option<Arc<Doorbell>>,
    ) -> impl Fn(&Appended<'_>) + Send + Sync + 'static {
        let announced = self.clone();
        move |record| {
            if record.recovery || record.via != Via::Local || ROUND.with(std::cell::Cell::get) {
                return;
            }
            announced.note(&record.table);
            if let Some(bell) = &bell {
                bell.ring();
            }
        }
    }
}

#[cfg(test)]
#[path = "announce_tests.rs"]
mod tests;
