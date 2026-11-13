// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-peer scheduling hints, leased only after the channel's actor is verified.
//! No mutex guard crosses network I/O. Durable inventories still decide delivery.

use super::Repair;
use crate::{Error, Result};
use astream_wire::sync::{Actor, Digest};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::{Deref, DerefMut},
    sync::{Condvar, Mutex},
    time::Instant,
};

/// Hints one endpoint keeps: one for each member a roster can hold and one more, as its witness
/// logs keep (`WITNESS_ENTRIES`). A cache smaller than the roster lets a peer's hint go before a
/// turn through the other peers comes back to it, so each round with that peer starts with none,
/// as its first did: its initiator asks for witnesses (`replica::WITNESS_GAP_MS`), which opens every
/// common table on both sides.
pub(super) const CAPACITY: usize = aterm_secrets::commons::roster::MAX_MEMBERS + 1;

#[derive(Default)]
struct Pool {
    active: BTreeMap<Actor, bool>,
    idle: VecDeque<(Actor, Repair)>,
    /// What [`Cache::announce`] named while each active peer's round ran: its hint takes it as
    /// the round releases it. One entry at most for each active peer.
    told: BTreeMap<Actor, (BTreeSet<Digest>, bool)>,
}

/// One local writer in one pinned trust context. Oldest idle hints are evicted
/// at capacity; a live lease is never evicted or shared by competing rounds.
#[derive(Default)]
pub(super) struct Cache(Mutex<Pool>, Condvar);

impl Cache {
    pub(super) fn acquire<'a>(&'a self, actor: Actor, repair: &'a mut Repair) -> Result<Lease<'a>> {
        self.acquire_on(actor, repair, false, Instant::now())
    }

    /// This side appended to `tables` (and, where `unnamed`, to one more it could not name):
    /// every peer's hint takes them ([`Repair::announce`]), so the next round this side
    /// initiates with each visits them first. An idle hint takes them now. A leased one takes
    /// them as its round releases it: that round may have read the table before the record,
    /// and its hint replaces the idle one. A peer with no hint here starts its next round with
    /// none, which asks which tables differ.
    pub(super) fn announce(&self, tables: &BTreeSet<Digest>, unnamed: bool) {
        let mut pool = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        let Pool { active, idle, told } = &mut *pool;
        for (_, hint) in idle.iter_mut() {
            hint.announce(tables, unnamed);
        }
        for actor in active.keys() {
            let (kept, kept_unnamed) = told.entry(*actor).or_default();
            // More tables than a round's list holds are kept as one unnamed.
            if kept.len() + tables.len() > crate::replica::MAX_TABLES {
                kept.clear();
                *kept_unnamed = true;
            } else {
                kept.extend(tables);
            }
            *kept_unnamed |= unnamed;
        }
    }

    /// Only competing authenticated channels use the preference. A lone
    /// channel in either direction always proceeds. Both endpoints derive the
    /// same preferred physical channel from their authenticated actor ordering.
    /// Waiting releases the cache mutex and holds no journal or network guard.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbPeerRepairCache",
            action = "Acquire",
            project = "aterm_db::control::peers::tests::cache_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbPeerRepairCache",
            action = "Duplicate",
            project = "aterm_db::control::peers::tests::cache_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbPeerRepairCache",
            action = "OtherAcquire",
            project = "aterm_db::control::peers::tests::cache_transition"
        )
    )]
    pub(super) fn acquire_on<'a>(
        &'a self,
        actor: Actor,
        repair: &'a mut Repair,
        preferred: bool,
        deadline: Instant,
    ) -> Result<Lease<'a>> {
        self.acquire_on_with(actor, repair, preferred, deadline, || {
            #[cfg(test)]
            super::tests::peer_cache_stage(super::tests::PeerCacheStage::Waiting);
        })
    }

    fn acquire_on_with<'a>(
        &'a self,
        actor: Actor,
        repair: &'a mut Repair,
        preferred: bool,
        deadline: Instant,
        mut waiting: impl FnMut(),
    ) -> Result<Lease<'a>> {
        let mut pool = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        while let Some(active_preferred) = pool.active.get(&actor) {
            if !preferred || *active_preferred {
                return Err(Error::new(
                    "Commons peer already has an active repair round",
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::new(
                    "Commons preferred repair waited until its exchange deadline",
                ));
            }
            waiting();
            // In crossed dials the other endpoint rejects its nonpreferred
            // contender, closing this active round's socket. Its ordinary
            // completion releases the lease; no channel is forcibly revoked.
            let (next, _) = self
                .1
                .wait_timeout(pool, remaining)
                .unwrap_or_else(|poison| poison.into_inner());
            pool = next;
        }
        if let Some(index) = pool.idle.iter().position(|(id, _)| *id == actor) {
            let (_, saved) = pool.idle.remove(index).unwrap();
            *repair = saved;
        } else if pool.active.len() + pool.idle.len() == CAPACITY && pool.idle.pop_front().is_none()
        {
            return Err(Error::new("Commons peer repair capacity is busy"));
        }
        pool.active.insert(actor, preferred);
        Ok(Lease {
            cache: self,
            actor,
            repair,
        })
    }
}

pub(super) struct Lease<'a> {
    cache: &'a Cache,
    actor: Actor,
    repair: &'a mut Repair,
}
impl Deref for Lease<'_> {
    type Target = Repair;
    fn deref(&self) -> &Repair {
        self.repair
    }
}
impl DerefMut for Lease<'_> {
    fn deref_mut(&mut self) -> &mut Repair {
        self.progress()
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.release();
    }
}
impl Lease<'_> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbPeerRepairCache",
            action = "Advance",
            project = "aterm_db::control::peers::tests::cache_transition"
        )
    )]
    fn progress(&mut self) -> &mut Repair {
        self.repair
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbPeerRepairCache",
            action = "Release",
            project = "aterm_db::control::peers::tests::cache_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbPeerRepairCache",
            action = "OtherRelease",
            project = "aterm_db::control::peers::tests::cache_transition"
        )
    )]
    fn release(&mut self) {
        // The caller retains its report; the endpoint retains only a hint copy.
        let mut saved = self.repair.clone();
        saved.application = None;
        let mut pool = self
            .cache
            .0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        pool.active.remove(&self.actor);
        if let Some((tables, unnamed)) = pool.told.remove(&self.actor) {
            saved.announce(&tables, unnamed);
        }
        pool.idle.push_back((self.actor, saved));
        drop(pool);
        self.cache.1.notify_all();
    }
}

#[cfg(test)]
mod tests;
