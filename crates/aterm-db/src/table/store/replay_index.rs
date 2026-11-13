// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A store open's admission of each replayed header to the causal index, with no index built: the
//! open holds the records, so what an index would add is the discarded sources and a count.

use astream_wire::sync::{Digest, Header, Refusal, sync_within_capacity};
use std::collections::{
    BTreeMap,
    btree_map::{Entry, VacantEntry},
};

/// What [`Index::insert`](astream_wire::sync::Index::insert) would answer, header by header, to
/// an index given each header a store open admits, in turn: the operations the open holds so far
/// (the caller's map, whose headers this reads) and the discarded sources it keeps here. It answers
/// in insert's order: the shape ([`Header::is_shaped`]), then the header held under the id, then
/// the capacity.
pub(super) struct ReplayIndex {
    /// The discarded sources admitted under an id no operation held then.
    sources: BTreeMap<Digest, Header>,
    /// How many ids the index would hold: every operation's and source's, each counted once.
    distinct: usize,
    capacity: usize,
}

impl ReplayIndex {
    /// An empty index's admission under `capacity`
    /// ([`Index::new`](astream_wire::sync::Index::new)).
    pub(super) const fn new(capacity: usize) -> Self {
        Self {
            sources: BTreeMap::new(),
            distinct: 0,
            capacity,
        }
    }

    /// [`Index::insert`](astream_wire::sync::Index::insert)'s answer to `header`, where `held` is
    /// the operation held under its id: `Ok(true)` for an id the index would not hold yet,
    /// `Ok(false)` for an equal header held under it.
    fn admits(&self, held: Option<&Header>, header: &Header) -> Result<bool, Refusal> {
        if !header.is_shaped() {
            return Err(Refusal::Shape);
        }
        match held.or_else(|| self.sources.get(&header.id)) {
            Some(old) if old == header => Ok(false),
            Some(_) => Err(Refusal::DigestCollision),
            None if sync_within_capacity(self.distinct as u64, self.capacity as u64) => Ok(true),
            None => Err(Refusal::Capacity),
        }
    }

    /// Admit `source`, a discarded source a conflict certificate keeps, where `records` holds the
    /// operations admitted so far and `header_of` reads each one's header.
    pub(super) fn source<V>(
        &mut self,
        records: &BTreeMap<Digest, V>,
        header_of: impl Fn(&V) -> Option<&Header>,
        source: &Header,
    ) -> Result<(), Refusal> {
        if self.admits(records.get(&source.id).and_then(header_of), source)? {
            self.distinct += 1;
            self.sources.insert(source.id, source.clone());
        }
        Ok(())
    }

    /// Admit operation `header`, where `slot` is its id's entry among the operations admitted so
    /// far and `header_of` reads each one's header: the vacant entry the caller holds it in, or
    /// `None` where an equal header is held there already.
    pub(super) fn operation<'m, V>(
        &mut self,
        slot: Entry<'m, Digest, V>,
        header_of: impl Fn(&V) -> &Header,
        header: &Header,
    ) -> Result<Option<VacantEntry<'m, Digest, V>>, Refusal> {
        match slot {
            Entry::Occupied(held) => {
                self.admits(Some(header_of(held.get())), header)?;
                Ok(None)
            }
            Entry::Vacant(slot) => {
                if self.admits(None, header)? {
                    self.distinct += 1;
                }
                Ok(Some(slot))
            }
        }
    }
}

#[cfg(test)]
mod tests;
