// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A typed table's checkpoints (docs/DESIGN-table-checkpoint-2026-11-15.md, tracker LT-B10B):
//! what state a checkpoint of a typed table certifies, and the pass that signs one.
//!
//! The state is what [`View::read`] makes of the records at the cut: its cells (claims
//! interpreted), its owned collections' rows and the records past the table's quota. Each part
//! is a function of the cut and of the control history every member holds, so two writers at
//! one cut digest the same state; what a view says of this Mac alone (what it awaits, what it
//! refuses) is not part of it.
//!
//! The pass ([`Handle::checkpoint`]) runs from the node service's hearing tick, once per table,
//! and signs at most one checkpoint each time. While any current member holds the version fence
//! ([`hearing::CHECKPOINTS`]) it signs nothing. Otherwise it seconds the newest share another
//! writer signed that this Mac computes identically, or proposes its own once the cut has grown
//! by [`STRIDE`] records past every one it shared, so a table gains one checkpoint record per
//! writer per [`STRIDE`] of its own.

use super::{Handle, Table, View, decode};
use crate::{
    Result,
    checkpoint::{self, Body, History},
    documents::{application, log::Log},
};
use astream_wire::sync::{Actor, Digest};
use aterm_secrets::commons::{
    bootstrap::Bootstrap,
    finality,
    hearing::{self, Ledger},
};
use std::collections::{BTreeMap, BTreeSet};

/// How much older than the proposing Mac's clock a record must be to go below a cut: a
/// checkpoint never races what is still arriving (design §3).
pub(crate) const AGE_MS: u64 = 14 * 24 * 60 * 60 * 1000;

/// How many records a cut must hold beyond every one this Mac shared before it proposes another.
pub(crate) const STRIDE: u64 = 1024;

/// Domain separation for a typed table's state digest.
const STATE_DOMAIN: &[u8] = b"aterm/att/checkpoint-state/1\0";

/// SHA-256 of the typed table's state at `cut` (a causally closed set of `log`'s records).
///
/// # Errors
/// The view's resolution refuses (as [`super::Resolved::read`]), a cell candidate has no header
/// here, or a value does not encode.
pub(crate) fn state(
    table: &'static Table,
    session: &crate::table::Session<'_>,
    log: &Log,
    cut: &BTreeSet<Digest>,
) -> Result<Digest> {
    let view = View::read(table, session, &log.as_of_cut(cut))?;
    let index = log.causal_index();
    let cells = checkpoint::cells_digest(&view.cells, |id| {
        index.get(id).map(|header| (header.actor, header.sequence))
    })?;
    let mut hash = aterm_digest::Sha256::new();
    hash.update(STATE_DOMAIN);
    hash.update(cells);
    hash.update((view.owned.len() as u64).to_le_bytes());
    for (collection, read) in &view.owned {
        put_text(&mut hash, collection);
        hash.update((read.inert as u64).to_le_bytes());
        hash.update((read.rows.len() as u64).to_le_bytes());
        for (key, row) in &read.rows {
            put_text(&mut hash, key);
            hash.update(row.machine);
            hash.update(row.actor);
            let value = aterm_json::to_vec(&row.value)
                .map_err(|_| crate::Error::new("an owned row's value does not encode"))?;
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
            for part in [row.rank.0, row.rank.1, row.rank.2] {
                hash.update(part.to_le_bytes());
            }
            hash.update(row.operation);
        }
    }
    hash.update((view.quota.over.len() as u64).to_le_bytes());
    for id in &view.quota.over {
        hash.update(id);
    }
    Ok(hash.finalize())
}

fn put_text(hash: &mut aterm_digest::Sha256, text: &str) {
    hash.update((text.len() as u64).to_le_bytes());
    hash.update(text.as_bytes());
}

/// Whether `later`'s cut holds `earlier`'s: entrywise at or past it.
fn contains(later: &Body, earlier: &Body) -> bool {
    earlier.frontier.iter().all(|(actor, tip)| {
        later
            .frontier
            .get(actor)
            .is_some_and(|at| at.sequence >= tip.sequence)
    })
}

/// What the pass did in one table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signed {
    /// A current member holds the version fence: nothing is signed.
    Fenced,
    /// Nothing is due.
    Nothing,
    /// This Mac signed another writer's body, which it computes identically.
    Seconded,
    /// This Mac proposed a body of its own.
    Proposed,
}

impl Handle {
    /// Sign at most one checkpoint of this table (the module's pass): none while a current member
    /// holds the fence; else the newest share of another writer this Mac computes identically and
    /// has not signed, whose cut holds every one this Mac shared; else, at `now_ms`, its own,
    /// once the cut holds [`STRIDE`] records more than every one it shared.
    ///
    /// # Errors
    /// No Commons, the table's storage cannot be read or written, or the record cannot be signed.
    pub fn checkpoint(&self, ledger: &Ledger, now_ms: u64) -> Result<Signed> {
        self.checkpoint_every(ledger, now_ms, STRIDE)
    }

    /// [`Self::checkpoint`] with `stride` in place of [`STRIDE`].
    ///
    /// # Errors
    /// As [`Self::checkpoint`].
    pub(crate) fn checkpoint_every(
        &self,
        ledger: &Ledger,
        now_ms: u64,
        stride: u64,
    ) -> Result<Signed> {
        let Ok(Some(bootstrap)) = Bootstrap::cached(&self.paths) else {
            return Ok(Signed::Nothing);
        };
        let Ok(Some(access)) = bootstrap.access() else {
            return Ok(Signed::Nothing);
        };
        let held = ledger.holding_below(
            access.members().iter().map(|member| member.binding()),
            now_ms,
            hearing::CHECKPOINTS,
        );
        if !held.is_empty() {
            return Ok(Signed::Fenced);
        }
        let members: BTreeSet<Actor> = access
            .members()
            .iter()
            .map(|member| member.binding().actor())
            .collect();
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            false,
            self.descriptor(),
            |session, store, scope| {
                let log = Log::read_with(session, store, scope, decode)?;
                let me = session.actor();
                let authored = |id: &Digest| log.authored_ms(id);
                let state = |cut: &BTreeSet<Digest>| state(table, session, &log, cut);
                let history = History {
                    scope: scope.digest(),
                    generation: scope.generation(),
                    index: log.causal_index(),
                    projection: log.projection(),
                    authored: &authored,
                    state: &state,
                };
                let shares: Vec<(Actor, Body)> = store
                    .checkpoints()
                    .map(|(signer, body)| (signer, body.clone()))
                    .filter(|(_, body)| body.scope == history.scope)
                    .collect();
                let mine: Vec<&Body> = shares
                    .iter()
                    .filter(|(signer, _)| *signer == me)
                    .map(|(_, body)| body)
                    .collect();
                let nests = |body: &Body| mine.iter().all(|earlier| contains(body, earlier));
                let mut theirs: Vec<&Body> = shares
                    .iter()
                    .filter(|(signer, body)| *signer != me && !mine.contains(&body) && nests(body))
                    .map(|(_, body)| body)
                    .collect();
                theirs.sort_by_key(|body| std::cmp::Reverse(body.below));
                for body in theirs {
                    if let Ok(computed) = checkpoint::second(&history, body) {
                        store.sign_checkpoint(session, &computed, &held)?;
                        return Ok(Signed::Seconded);
                    }
                }
                let writers = current_writers(&log, &members, now_ms);
                let Some(computed) =
                    checkpoint::compute(&history, &writers, now_ms.saturating_sub(AGE_MS))
                else {
                    return Ok(Signed::Nothing);
                };
                let shared = mine.iter().map(|body| body.below).max().unwrap_or(0);
                if !nests(computed.body()) || computed.body().below < shared.saturating_add(stride)
                {
                    return Ok(Signed::Nothing);
                }
                store.sign_checkpoint(session, &computed, &held)?;
                Ok(Signed::Proposed)
            },
        )
    }
}

/// The table's current writers (design §3): members whose newest record here was authored
/// within [`finality::HEARD_WITHIN_MS`] of `now_ms`.
fn current_writers(log: &Log, members: &BTreeSet<Actor>, now_ms: u64) -> BTreeSet<Actor> {
    let mut newest: BTreeMap<Actor, u64> = BTreeMap::new();
    for id in &log.projection().ordered {
        let (Some(header), Some(at)) = (log.causal_index().get(id), log.authored_ms(id)) else {
            continue;
        };
        if members.contains(&header.actor) {
            let entry = newest.entry(header.actor).or_insert(at);
            *entry = (*entry).max(at);
        }
    }
    let writers: BTreeSet<Actor> = newest.keys().copied().collect();
    finality::recent(&writers, now_ms, |actor| newest.get(actor).copied())
}

/// [`Handle::checkpoint`] for every typed table this build compiles, at the generation this
/// Mac's catalog uses: how many it signed. A table it cannot read or write is passed over; the
/// next tick tries it again.
pub fn sign_due(paths: &aterm_secrets::client::Paths, ledger: &Ledger, now_ms: u64) -> usize {
    let mut handles: Vec<Handle> = Vec::new();
    for compiled in super::TABLES {
        let handle = Handle::at(paths.clone(), compiled);
        if !handles
            .iter()
            .any(|held| std::ptr::eq(held.compiled, handle.compiled))
        {
            handles.push(handle);
        }
    }
    handles
        .into_iter()
        .filter(|handle| {
            matches!(
                handle.checkpoint(ledger, now_ms),
                Ok(Signed::Seconded | Signed::Proposed)
            )
        })
        .count()
}
