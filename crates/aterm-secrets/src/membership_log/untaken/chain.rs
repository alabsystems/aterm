// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A records server's log as raw frames, for tests: it holds records this build cannot read (a
//! kind only a newer build writes), whose parents the test names, and answers a pull as
//! [`crate::membership_log::Store::missing`] does, without the framer under test.

use crate::membership_log::{Record, RecordId};
use std::collections::BTreeSet;

/// Frames in the order a server holds them: each one's id (SHA-256 of its bytes), the parents
/// the test built it with, and its bytes.
#[derive(Clone, Default)]
pub(crate) struct Chain(Vec<(RecordId, Vec<RecordId>, Vec<u8>)>);

impl Chain {
    /// A chain holding `records`, in order.
    pub(crate) fn of<'a>(records: impl IntoIterator<Item = &'a Record>) -> Self {
        let mut chain = Self::default();
        for record in records {
            chain.push(record);
        }
        chain
    }

    /// One more record this build reads.
    pub(crate) fn push(&mut self, record: &Record) {
        self.0.push((
            *record.id(),
            record.parents().to_vec(),
            record.bytes().to_vec(),
        ));
    }

    /// One more frame, `bytes`, whose parents are `parents`, returning its id.
    pub(crate) fn push_raw(&mut self, bytes: Vec<u8>, parents: Vec<RecordId>) -> RecordId {
        let id = aterm_digest::Sha256::digest(&bytes);
        self.0.push((id, parents, bytes));
        id
    }

    /// Drop the frame `id`.
    pub(crate) fn drop_frame(&mut self, id: &RecordId) {
        self.0.retain(|(held, _, _)| held != id);
    }

    /// Whether the chain holds `id`.
    pub(crate) fn holds(&self, id: &RecordId) -> bool {
        self.0.iter().any(|(held, _, _)| held == id)
    }

    /// Whether `have` names a frame the chain does not hold (`X-Aterm-Records-Need`).
    pub(crate) fn lacks_any(&self, have: &[RecordId]) -> bool {
        have.iter().any(|id| !self.holds(id))
    }

    /// The frames past the causal closure of the ids `have` names that the chain holds, in order.
    pub(crate) fn past(&self, have: &[RecordId]) -> Vec<u8> {
        let mut known = BTreeSet::new();
        let mut stack: Vec<RecordId> = have.iter().filter(|id| self.holds(id)).copied().collect();
        while let Some(id) = stack.pop() {
            if known.insert(id)
                && let Some((_, parents, _)) = self.0.iter().find(|(held, _, _)| *held == id)
            {
                stack.extend(parents.iter().copied());
            }
        }
        self.0
            .iter()
            .filter(|(id, _, _)| !known.contains(id))
            .flat_map(|(_, _, bytes)| bytes.iter().copied())
            .collect()
    }
}

/// The record ids an HTTP request line's `have=` names.
pub(crate) fn have_named(line: &str) -> Vec<RecordId> {
    let target = line.split(' ').nth(1).unwrap_or("");
    let Some((_, query)) = target.split_once("have=") else {
        return Vec::new();
    };
    query
        .split(',')
        .filter_map(|hex| {
            let mut id = [0u8; 32];
            aterm_codec::hex::decode_into(hex.as_bytes(), &mut id).then_some(id)
        })
        .collect()
}

/// Whether `body` holds `frame` (a record's bytes) whole.
pub(crate) fn carries(body: &[u8], frame: &[u8]) -> bool {
    !frame.is_empty() && body.windows(frame.len()).any(|window| window == frame)
}
