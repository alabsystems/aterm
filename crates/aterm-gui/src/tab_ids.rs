// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tab IDs this Mac knows, and the one minter that keeps a new tab's ID unlike every one
//! of them (LT-01: an ID is unique across the fleet).
//!
//! Known is: every ID this process minted, adopted or holds for a restore; every ID a session
//! on this Mac ever claimed (`claims/<id>` outlives its claim); and every ID the fleet glance
//! last named for another Mac. A draw that begins with the shown form ([`TabId::short`]) of one
//! is drawn again ([`TabId::mint`]), so the five characters a tab is shown by and typed as name
//! it alone among every ID this Mac knew at its mint (LT-20); [`shown`] lengthens the form of
//! two that share them anyway (minted on two Macs before either saw the other's). The
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
    fn ids(&self) -> impl Iterator<Item = &TabId> {
        self.here.iter().chain(&self.reserved).chain(&self.fleet)
    }

    /// Whether a known ID is shown as `id` would be: the same ID, or one that shares its first
    /// five characters.
    fn holds(&self, id: &TabId) -> bool {
        self.ids().any(|known| known.short() == id.short())
    }
}

static KNOWN: Mutex<Option<Known>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Known) -> R) -> R {
    let mut guard = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(Known::default))
}

/// Mint a new tab's ID: one no tab this Mac or the fleet knows holds or begins as it does
/// ([`TabId::short`]). The minted ID is known from then on.
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
    let sid = SessionId::from_tab(id);
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

/// `sid` as a person reads it: a tab ID at its first five characters, longer only as far as it
/// takes to tell it apart from another ID this Mac knows that shares them ([`TabId::shown`]),
/// and any other word as the hand slot cuts it.
pub(crate) fn shown(sid: &str) -> String {
    match TabId::parse(sid) {
        Some(id) => with(|k| id.shown(k.ids()).to_owned()),
        None => aterm_messages::presence::short_sid(sid),
    }
}

/// Whether `id` is one the minter would refuse.
#[cfg(test)]
pub(crate) fn known(id: &TabId) -> bool {
    with(|k| k.holds(id)) || crate::identity_claim::ever_claimed(id)
}

#[cfg(test)]
#[path = "tab_ids_tests.rs"]
mod tests;
