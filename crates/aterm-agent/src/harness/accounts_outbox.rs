// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded local publication intents. All durable edits use the account receipt;
//! a queue entry is not authority to access its retained destination.

use super::Store;
use aterm_db::accounts::publication::{Completion, Draft, Pending};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub(super) const MAX_BYTES: u64 = 256 * 1024;
const MAX_ENTRIES: usize = 256;
const FILE: &str = "publication-outbox.json";
const WITNESS: &str = "publication-outbox.seen";

#[cfg(test)]
thread_local! {
    pub(super) static MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// One immutable entry. Its ordinal is local to its account store; the pending
/// command independently binds its network, table, writer and retry identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    ordinal: u64,
    pending: Pending,
}

impl Entry {
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }
    pub fn pending(&self) -> &Pending {
        &self.pending
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Queue {
    schema: u8,
    last: u64,
    entries: Vec<Entry>,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            schema: 1,
            last: 0,
            entries: Vec::new(),
        }
    }
}

impl Queue {
    pub(super) fn decode(text: &str) -> Result<Self, String> {
        if text.len() as u64 > MAX_BYTES {
            return Err("publication outbox exceeds its limit".into());
        }
        let queue: Self = aterm_json::from_str(text).map_err(|_| "invalid publication outbox")?;
        queue.encode()?;
        Ok(queue)
    }

    pub(super) fn encode(&self) -> Result<String, String> {
        if self.schema != 1 || self.entries.len() > MAX_ENTRIES {
            return Err("invalid publication outbox schema or capacity".into());
        }
        let mut previous = 0;
        let mut identities = BTreeSet::new();
        for entry in &self.entries {
            if entry.ordinal <= previous || entry.ordinal > self.last {
                return Err("publication outbox ordinals are not increasing".into());
            }
            previous = entry.ordinal;
            entry.pending.encode().map_err(|why| why.to_string())?;
            let destination = aterm_json::to_string(entry.pending.destination())
                .map_err(|why| why.to_string())?;
            let request = entry.pending.request().map_err(|why| why.to_string())?;
            if !identities.insert((destination, request)) {
                return Err("duplicate publication outbox intent".into());
            }
        }
        let text =
            aterm_json::to_string(self).map_err(|_| "publication outbox encoding failed")? + "\n";
        if text.len() as u64 > MAX_BYTES {
            return Err("publication outbox exceeds its limit".into());
        }
        Ok(text)
    }

    pub(super) fn preserves_ordinals_after(&self, before: &Self) -> Result<(), String> {
        if self.last < before.last
            || self.entries.iter().any(|entry| {
                entry.ordinal <= before.last && !before.entries.iter().any(|old| old == entry)
            })
        {
            return Err("publication outbox reuses a retained ordinal".into());
        }
        Ok(())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountPublicationOutbox",
            action = "Scenario",
            reason = "The environment selects a receipt cut, full queue, or stale completion."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountPublicationOutbox",
            action = "Check",
            project = "aterm_agent::harness::accounts::outbox::tests::transition"
        )
    )]
    pub(super) fn enqueue(&mut self, draft: Draft) -> Result<Entry, String> {
        if self.entries.len() >= MAX_ENTRIES {
            #[cfg(test)]
            if MUTANT.get() == 2 {
                self.entries.remove(0);
            } else {
                return Err("publication outbox is full".into());
            }
            #[cfg(not(test))]
            return Err("publication outbox is full".into());
        }
        let ordinal = self
            .last
            .checked_add(1)
            .ok_or("publication outbox ordinal exhausted")?;
        let entry = Entry {
            ordinal,
            pending: draft.allocate().map_err(|why| why.to_string())?,
        };
        let mut after = self.clone();
        after.last = ordinal;
        after.entries.push(entry.clone());
        after.encode()?;
        *self = after;
        Ok(entry)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountPublicationOutbox",
            action = "Check",
            project = "aterm_agent::harness::accounts::outbox::tests::transition"
        )
    )]
    pub(super) fn acknowledge(
        &mut self,
        expected: &Entry,
        completion: &Completion,
    ) -> Result<bool, String> {
        if !completion.matches(&expected.pending) {
            return Err("publication completion names another intent".into());
        }
        if expected.ordinal == 0 || expected.ordinal > self.last {
            return Err("publication completion has an unallocated ordinal".into());
        }
        if self
            .entries
            .iter()
            .any(|entry| entry.ordinal == expected.ordinal && entry.pending != expected.pending)
        {
            return Err("publication completion changes a retained intent".into());
        }
        let at = self.entries.iter().position(|entry| entry == expected);
        #[cfg(test)]
        let at = at.or_else(|| (MUTANT.get() == 3 && !self.entries.is_empty()).then_some(0));
        let Some(at) = at else {
            return Ok(false);
        };
        self.entries.remove(at);
        Ok(true)
    }
}

#[cfg(test)]
#[path = "accounts_outbox_tests.rs"]
mod tests;

impl Store {
    pub(super) fn outbox_file(&self) -> PathBuf {
        self.dir.join(FILE)
    }
    pub(super) fn outbox_witness(&self) -> PathBuf {
        self.dir.join(WITNESS)
    }

    /// The caller supplies the account read fence or holds its writer.
    pub(super) fn outbox_unfenced(&self) -> Result<(Option<String>, Queue), String> {
        let text = super::sync_commit::read(&self.outbox_file(), MAX_BYTES)?;
        let witness = super::sync_commit::read(&self.outbox_witness(), 0)?;
        match (text.as_deref(), witness.as_deref()) {
            (None, None) => Ok((None, Queue::default())),
            (Some(text), Some("")) => Ok((Some(text.to_owned()), Queue::decode(text)?)),
            _ => Err("publication outbox or retained witness is missing".into()),
        }
    }

    /// Read committed intents only. A paired pending account receipt must be
    /// recovered with its caller's independent roster scope before dispatch.
    pub fn pending_publications(&self) -> Result<Vec<Entry>, String> {
        self.recover_state_records()?;
        let receipt = self.sync_read_receipt()?;
        let (_, queue) = self.outbox_unfenced()?;
        self.check_sync_read_receipt(&receipt)?;
        Ok(queue.entries)
    }
}
