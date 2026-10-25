// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-peer scheduling hints, leased only after the channel's actor is verified.
//! No mutex guard crosses network I/O. Durable inventories still decide delivery.

use super::Repair;
use crate::{Error, Result};
use astream_wire::sync::Actor;
use std::{
    collections::{BTreeSet, VecDeque},
    ops::{Deref, DerefMut},
    sync::Mutex,
};

const CAPACITY: usize = 64;

#[derive(Default)]
struct Pool {
    active: BTreeSet<Actor>,
    idle: VecDeque<(Actor, Repair)>,
}

/// One endpoint, hence one pinned trust context. Oldest idle hints are evicted
/// at capacity; a live lease is never evicted or shared by competing rounds.
#[derive(Default)]
pub(super) struct Cache(Mutex<Pool>);

impl Cache {
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
    pub(super) fn acquire<'a>(&'a self, actor: Actor, repair: &'a mut Repair) -> Result<Lease<'a>> {
        let mut pool = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        if pool.active.contains(&actor) {
            return Err(Error::new(
                "Commons peer already has an active repair round",
            ));
        }
        if let Some(index) = pool.idle.iter().position(|(id, _)| *id == actor) {
            let (_, saved) = pool.idle.remove(index).unwrap();
            *repair = saved;
        } else if pool.active.len() + pool.idle.len() == CAPACITY && pool.idle.pop_front().is_none()
        {
            return Err(Error::new("Commons peer repair capacity is busy"));
        }
        pool.active.insert(actor);
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
        pool.idle.push_back((self.actor, saved));
    }
}

#[cfg(test)]
mod tests;
