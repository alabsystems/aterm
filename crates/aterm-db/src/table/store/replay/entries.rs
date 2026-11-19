// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compact immutable journal records, separate from their custody chain.
//! A complete replay advances through bounded immutable blocks, releasing consumed
//! blocks when their other owners retire. Partial ranges own only selected entries.
//! No replay owns the custody chain.

#[cfg(test)]
use super::Receipt;
use super::{Entry, Linked, Witness, identify_with};
use crate::{
    Error,
    table::signatures::{Facts, Identified},
};
use astream_broker::BrokerRecord;
use astream_wire::sync::Digest;
use std::{ops::Range, sync::Arc};

#[derive(Clone)]
pub(in crate::table::store) struct Record {
    wire: Identified,
    received_ms: u64,
    conflict: bool,
}

const _: () = assert!(size_of::<Record>() < size_of::<Entry>());

pub(in crate::table::store) struct Block {
    records: Box<[Record]>,
    next: Option<Arc<Block>>,
}

impl Drop for Block {
    fn drop(&mut self) {
        // A large journal can have many small blocks. Retire an exclusively
        // owned suffix iteratively rather than recursing through Arc::drop.
        while let Some(next) = self.next.take() {
            match Arc::try_unwrap(next) {
                Ok(mut next) => self.next = next.next.take(),
                Err(_) => break,
            }
        }
    }
}

enum Records {
    Flat(Box<[Record]>),
    Blocks { head: Arc<Block>, len: usize },
}

impl Records {
    fn new(mut records: Vec<Record>) -> Self {
        let len = records.len();
        let margin = len * (size_of::<Entry>() - size_of::<Record>());
        let overhead = size_of::<Block>() + 2 * size_of::<usize>();
        let budget = margin / 2;
        if budget < overhead {
            return Self::Flat(records.into_boxed_slice());
        }
        // Split the descriptor saving between block overhead and retained
        // consumed wires. Partition once, with no payload-dependent retries.
        let mut ends = Vec::new();
        let mut prefix = 0usize;
        for (at, record) in records.iter().enumerate() {
            let bytes = record.wire.allocation_bytes();
            if prefix.saturating_add(bytes) > budget {
                // This last record is released as the cursor leaves the block,
                // so only the already consumed prefix needs retained room.
                ends.push(at + 1);
                prefix = 0;
            } else {
                prefix += bytes;
            }
        }
        if ends.last() != Some(&len) {
            ends.push(len);
        }
        if ends.len() * overhead > margin - budget {
            return Self::Flat(records.into_boxed_slice());
        }
        let mut head = None;
        for start in std::iter::once(&0)
            .chain(ends[..ends.len() - 1].iter())
            .rev()
        {
            let tail = records.split_off(*start).into_boxed_slice();
            head = Some(Arc::new(Block {
                records: tail,
                next: head,
            }));
        }
        Self::Blocks {
            head: head.unwrap(),
            len,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Flat(records) => records.len(),
            Self::Blocks { len, .. } => *len,
        }
    }

    fn iter(&self) -> RecordsIter<'_> {
        match self {
            Self::Flat(records) => RecordsIter {
                records: records.iter(),
                next: None,
                remaining: records.len(),
            },
            Self::Blocks { head, len } => RecordsIter::block(head, *len),
        }
    }

    fn get(&self, mut at: usize) -> Option<&Record> {
        match self {
            Self::Flat(records) => records.get(at),
            Self::Blocks { head, .. } => {
                let mut block = head.as_ref();
                loop {
                    if at < block.records.len() {
                        return block.records.get(at);
                    }
                    at -= block.records.len();
                    block = block.next.as_deref()?;
                }
            }
        }
    }

    fn allocation_bytes(&self) -> usize {
        match self {
            Self::Flat(records) => size_of_val(records.as_ref()),
            Self::Blocks { head, len } => {
                let mut blocks = 0;
                let mut next = Some(head.as_ref());
                while let Some(block) = next {
                    blocks += 1;
                    next = block.next.as_deref();
                }
                len * size_of::<Record>() + blocks * (size_of::<Block>() + 2 * size_of::<usize>())
            }
        }
    }
}

pub(in crate::table::store) struct RecordsIter<'a> {
    records: std::slice::Iter<'a, Record>,
    next: Option<&'a Block>,
    remaining: usize,
}

impl<'a> RecordsIter<'a> {
    fn block(block: &'a Block, remaining: usize) -> Self {
        Self {
            records: block.records.iter(),
            next: block.next.as_deref(),
            remaining,
        }
    }
}

impl<'a> Iterator for RecordsIter<'a> {
    type Item = &'a Record;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(record) = self.records.next() {
                self.remaining -= 1;
                return Some(record);
            }
            let next = self.next?;
            self.records = next.records.iter();
            self.next = next.next.as_deref();
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for RecordsIter<'_> {
    fn len(&self) -> usize {
        self.remaining
    }
}

pub(in crate::table::store) struct Links {
    records: Records,
    chain: Box<[Digest]>,
}

#[cfg(test)]
pub(in crate::table::store) struct LinkRef<'a> {
    pub wire: &'a Identified,
    pub received_ms: u64,
    pub conflict: bool,
    pub through: Witness,
}

impl Links {
    pub(in crate::table::store) fn empty() -> Self {
        Self {
            records: Records::Flat(Box::new([])),
            chain: Box::new([]),
        }
    }

    /// Continue only identified, contiguous records. A refusal leaves exactly
    /// the admitted prefix, as identify_onto does.
    pub(in crate::table::store) fn identify(
        before: Option<&Self>,
        records: Vec<BrokerRecord>,
        facts: Option<&Facts>,
    ) -> (Self, Option<Error>) {
        let prior = before.map_or(0, Self::len);
        let capacity = prior.saturating_add(records.len());
        let mut values = Vec::with_capacity(capacity);
        let mut chain = Vec::with_capacity(capacity);
        let mut through = Witness::default();
        if let Some(before) = before {
            values.extend(before.records.iter().cloned());
            chain.extend_from_slice(&before.chain);
            through = prior
                .checked_sub(1)
                .and_then(|at| before.witness(at))
                .unwrap_or_default();
        }
        let refused = records
            .into_iter()
            .try_for_each(|record| {
                let Linked {
                    wire,
                    received_ms,
                    conflict,
                    through: next,
                } = identify_with(record, through, facts)?;
                through = next;
                values.push(Record {
                    wire,
                    received_ms,
                    conflict,
                });
                chain.push(through.bytes()[8..].try_into().unwrap());
                Ok::<_, Error>(())
            })
            .err();
        (
            Self {
                records: Records::new(values),
                chain: chain.into_boxed_slice(),
            },
            refused,
        )
    }

    pub(in crate::table::store) fn len(&self) -> usize {
        self.records.len()
    }

    #[cfg(test)]
    pub(in crate::table::store) fn is_empty(&self) -> bool {
        self.records.len() == 0
    }

    /// Counts are the implicit positions of a contiguous chain. Each digest
    /// came from identify_with, whose Witness::advance enforces the count bound.
    pub(in crate::table::store) fn witness(&self, at: usize) -> Option<Witness> {
        let hash = self.chain.get(at)?;
        let mut bytes = [0; 40];
        bytes[..8].copy_from_slice(&((at as u64) + 1).to_le_bytes());
        bytes[8..].copy_from_slice(hash);
        Some(Witness::decode(&bytes).expect("identified journal count is bounded"))
    }

    #[cfg(test)]
    pub(in crate::table::store) fn get(&self, at: usize) -> Option<LinkRef<'_>> {
        let record = self.records.get(at)?;
        Some(LinkRef {
            wire: &record.wire,
            received_ms: record.received_ms,
            conflict: record.conflict,
            through: self
                .witness(at)
                .expect("each identified record has a chain link"),
        })
    }

    #[cfg(test)]
    pub(in crate::table::store) fn iter(&self) -> impl ExactSizeIterator<Item = LinkRef<'_>> {
        (0..self.len()).map(|at| self.get(at).unwrap())
    }

    pub(in crate::table::store) fn wires(&self) -> impl Iterator<Item = &Identified> {
        self.records.iter().map(|record| &record.wire)
    }

    pub(in crate::table::store) fn operation_ids(
        &self,
        count: usize,
    ) -> impl Iterator<Item = Digest> + '_ {
        self.records
            .iter()
            .take(count)
            .filter(|record| !record.conflict)
            .map(|record| record.wire.id())
    }

    /// Charge every block and its reference counts, custody digests and wires
    /// in full. Records have no spare capacity or repeated offsets.
    pub(in crate::table::store) fn allocation_bytes(&self) -> usize {
        size_of::<Self>()
            + 2 * size_of::<usize>()
            + self.records.allocation_bytes()
            + self.chain.len() * size_of::<Digest>()
            + self
                .records
                .iter()
                .map(|record| record.wire.allocation_bytes())
                .sum::<usize>()
    }

    pub(super) fn entries(&self, range: Range<usize>) -> Entries {
        let count = range.len();
        if count == 0 {
            return Entries::Owned(Vec::new());
        }
        let offset = range.start as u64;
        if count == 1 {
            let record = self.records.get(range.start).unwrap();
            return Entries::Single(Entry {
                received_ms: record.received_ms,
                offset,
                wire: record.wire.clone(),
                conflict: record.conflict,
            });
        }
        if range.start == 0
            && range.end == self.len()
            && let Records::Blocks { head, len } = &self.records
        {
            return Entries::Shared {
                block: Arc::clone(head),
                len: *len,
            };
        }
        // A partial range cannot own excluded blocks. Its exact owning vector
        // also releases each selected wire as it is consumed.
        let mut entries = Vec::with_capacity(count);
        entries.extend(
            self.records
                .iter()
                .skip(range.start)
                .take(count)
                .enumerate()
                .map(|(at, record)| Entry {
                    received_ms: record.received_ms,
                    offset: offset + at as u64,
                    wire: record.wire.clone(),
                    conflict: record.conflict,
                }),
        );
        Entries::Owned(entries)
    }
}

pub(in crate::table::store) enum Entries {
    Owned(Vec<Entry>),
    Single(Entry),
    Shared { block: Arc<Block>, len: usize },
}

pub(in crate::table::store) struct EntryRef<'a> {
    #[cfg(test)]
    pub received_ms: u64,
    pub offset: u64,
    pub wire: &'a Identified,
    pub conflict: bool,
}

#[cfg(test)]
impl EntryRef<'_> {
    pub(in crate::table::store) fn receipt(&self) -> Receipt {
        Receipt {
            id: self.wire.id(),
            received_ms: self.received_ms,
            offset: self.offset,
        }
    }
}

impl From<Vec<Entry>> for Entries {
    fn from(entries: Vec<Entry>) -> Self {
        Self::Owned(entries)
    }
}

impl Entries {
    pub(in crate::table::store) fn len(&self) -> usize {
        match self {
            Self::Owned(entries) => entries.len(),
            Self::Single(_) => 1,
            Self::Shared { len, .. } => *len,
        }
    }

    pub(in crate::table::store) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(in crate::table::store) fn iter(&self) -> Iter<'_> {
        match self {
            Self::Owned(entries) => Iter::Owned(entries.iter()),
            Self::Single(entry) => Iter::Owned(std::slice::from_ref(entry).iter()),
            Self::Shared { block, len } => Iter::Shared {
                records: RecordsIter::block(block, *len),
                at: 0,
            },
        }
    }
}

pub(in crate::table::store) enum Iter<'a> {
    Owned(std::slice::Iter<'a, Entry>),
    Shared { records: RecordsIter<'a>, at: u64 },
}

impl<'a> Iterator for Iter<'a> {
    type Item = EntryRef<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Owned(entries) => entries.next().map(|entry| EntryRef {
                #[cfg(test)]
                received_ms: entry.received_ms,
                offset: entry.offset,
                wire: &entry.wire,
                conflict: entry.conflict,
            }),
            Self::Shared { records, at } => records.next().map(|record| {
                let offset = *at;
                *at += 1;
                EntryRef {
                    #[cfg(test)]
                    received_ms: record.received_ms,
                    offset,
                    wire: &record.wire,
                    conflict: record.conflict,
                }
            }),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.len();
        (count, Some(count))
    }
}

impl ExactSizeIterator for Iter<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Owned(entries) => entries.len(),
            Self::Shared { records, .. } => records.len(),
        }
    }
}

pub(in crate::table::store) enum IntoIter {
    Owned(std::vec::IntoIter<Entry>),
    Single(std::option::IntoIter<Entry>),
    Shared {
        block: Option<Arc<Block>>,
        offset: u64,
        at: usize,
        remaining: usize,
    },
}

impl Iterator for IntoIter {
    type Item = Entry;
    fn next(&mut self) -> Option<Entry> {
        match self {
            Self::Owned(entries) => entries.next(),
            Self::Single(entry) => entry.next(),
            Self::Shared {
                block,
                offset,
                at,
                remaining,
            } => {
                let held = block.as_ref()?;
                let record = &held.records[*at];
                let entry = Entry {
                    received_ms: record.received_ms,
                    offset: *offset,
                    wire: record.wire.clone(),
                    conflict: record.conflict,
                };
                *at += 1;
                *offset += 1;
                *remaining -= 1;
                if *at == held.records.len() {
                    *block = held.next.clone();
                    *at = 0;
                }
                Some(entry)
            }
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.len();
        (count, Some(count))
    }
}

impl ExactSizeIterator for IntoIter {
    fn len(&self) -> usize {
        match self {
            Self::Owned(entries) => entries.len(),
            Self::Single(entry) => entry.len(),
            Self::Shared { remaining, .. } => *remaining,
        }
    }
}

impl IntoIterator for Entries {
    type Item = Entry;
    type IntoIter = IntoIter;
    fn into_iter(self) -> IntoIter {
        match self {
            Self::Owned(entries) => IntoIter::Owned(entries.into_iter()),
            Self::Single(entry) => IntoIter::Single(Some(entry).into_iter()),
            Self::Shared { block, len } => IntoIter::Shared {
                block: Some(block),
                offset: 0,
                at: 0,
                remaining: len,
            },
        }
    }
}

impl<'a> IntoIterator for &'a Entries {
    type Item = EntryRef<'a>;
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}
