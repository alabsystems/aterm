// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tab IDs this Mac knows, and the one minter that keeps a new tab's ID unlike every one
//! of them (LT-01: an ID is unique across the fleet).
//!
//! Known is: every ID this process minted, adopted or holds for a restore; every ID a session
//! on this Mac ever claimed (`claims/<id>` outlives its claim); and every ID the fleet glance
//! last named for another Mac. A draw that hits one is drawn again ([`TabId::mint`]). The
//! fleet half is as complete as the fleet's published rows: a Mac that publishes none of its
//! tabs cannot be checked against, and 50 random bits keep that clash at odds of (tabs) / 2^50.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use aterm_session::SessionId;
use aterm_types::tab_id::TabId;

#[derive(Default)]
struct Known {
    /// Minted, adopted or kept by this process.
    here: HashSet<TabId>,
    /// Named by the restore layout this process is applying: never minted for another tab, and
    /// each kept for its own leaf ([`keep`]).
    reserved: HashSet<TabId>,
    /// Named by the fleet glance for other Macs, replaced whole on each read.
    fleet: HashSet<TabId>,
}

impl Known {
    fn holds(&self, id: &TabId) -> bool {
        self.here.contains(id) || self.reserved.contains(id) || self.fleet.contains(id)
    }
}

static KNOWN: Mutex<Option<Known>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Known) -> R) -> R {
    let mut guard = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(Known::default))
}

/// Mint a new tab's ID: one no tab this Mac or the fleet knows holds. The minted ID is known from
/// then on.
pub(crate) fn mint() -> SessionId {
    let sid =
        SessionId::mint(|id| with(|k| k.holds(id)) || crate::identity_claim::ever_claimed(id));
    note_here(&sid);
    sid
}

/// `sid` belongs to a tab this process holds (adopted, restored or minted elsewhere): no mint
/// may hand it out again. A name that is no tab ID is no mint's to clash with.
pub(crate) fn note_here(sid: &SessionId) {
    if let Some(id) = sid.tab() {
        with(|k| k.here.insert(id));
    }
}

/// The tab IDs a restore layout names, held for their leaves ([`keep`]).
pub(crate) fn reserve(ids: impl IntoIterator<Item = TabId>) {
    with(|k| k.reserved.extend(ids));
}

/// A cold restore's leaf ID `text`, kept for the shell that fills the leaf: `Some` when it is a tab
/// ID no tab of this process holds and this process could claim it (no live instance on this Mac
/// serves it; the claim is held from then on). `None` sends the leaf to a new ID ([`mint`]).
pub(crate) fn keep(text: &str) -> Option<SessionId> {
    let id = TabId::parse(text)?;
    if with(|k| k.here.contains(&id)) {
        return None;
    }
    let sid = SessionId::new(id.as_str());
    if !crate::identity_claim::claim_kept(&sid) {
        return None;
    }
    with(|k| {
        k.reserved.remove(&id);
        k.here.insert(id);
    });
    Some(sid)
}

/// The tab IDs the fleet glance names for other Macs now.
pub(crate) fn note_fleet(ids: impl IntoIterator<Item = TabId>) {
    let fleet: HashSet<TabId> = ids.into_iter().collect();
    with(|k| k.fleet = fleet);
}

/// Whether `id` is one the minter would refuse.
#[cfg(test)]
pub(crate) fn known(id: &TabId) -> bool {
    with(|k| k.holds(id)) || crate::identity_claim::ever_claimed(id)
}

#[cfg(test)]
#[path = "tab_ids_tests.rs"]
mod tests;
