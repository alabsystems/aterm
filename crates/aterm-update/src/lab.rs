// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lab's update records, as the updater reads them: each lab Mac's [`UpdateRecord`] row in
//! the lab model's journal (`alab/journal`'s `upd` collection). The updater does not depend on
//! the database: the executable installs a reader ([`install_lab`]) where a lab exists, and where
//! none is installed (no Commons, a public install, Windows, a test) there are no rows, so a
//! hold ends only by its schedule, a newer build or a person.

use std::sync::OnceLock;

use aterm_update_core::update_record::UpdateRecord;

/// One lab Mac's update record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabRow {
    /// The machine the row is about (its Commons uuid, or a label where the reader has one).
    pub machine: String,
    /// What that machine says of its own updates.
    pub record: UpdateRecord,
    /// Whether it is this Mac's own row, which is listed but is never evidence for this Mac's
    /// holds: it may lag the record it was published from.
    pub own: bool,
}

/// The lab's update records, as the executable reads them.
pub trait LabUpdates: Send + Sync {
    /// Every lab Mac's live row, this one's included, or `None` where this Mac holds no lab it
    /// is a member of (or the rows could not be read just then).
    fn rows(&self) -> Option<Vec<LabRow>>;
}

static LAB: OnceLock<Box<dyn LabUpdates>> = OnceLock::new();

/// Install the lab reader. The first install wins; returns whether this one did.
pub fn install_lab(lab: Box<dyn LabUpdates>) -> bool {
    LAB.set(lab).is_ok()
}

/// Every lab Mac's live row, or `None` with no lab reader or no lab.
#[must_use]
pub fn rows() -> Option<Vec<LabRow>> {
    LAB.get()?.rows()
}

/// The records of the lab's other Macs, as evidence for [`lift_by`] (empty with no lab).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[must_use]
pub(crate) fn peers() -> Vec<UpdateRecord> {
    peer_records(rows().unwrap_or_default())
}

/// The lab's other Macs' records when they can matter to `record` at `now`: only while it
/// holds an artifact no peer has cut short yet. Otherwise none, and the journal is not read:
/// a check runs every few minutes, and most find nothing held.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[must_use]
pub(crate) fn peers_for(record: &UpdateRecord, now: u64) -> Vec<UpdateRecord> {
    if record.live_hold(now).is_some_and(|hold| !hold.peer_lifted) {
        peers()
    } else {
        Vec::new()
    }
}

/// The records among `rows` that are another Mac's: this Mac's own row is no peer.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn peer_records(rows: Vec<LabRow>) -> Vec<UpdateRecord> {
    rows.into_iter()
        .filter(|row| !row.own)
        .map(|row| row.record)
        .collect()
}

/// Cut `record`'s hold short on the first of `peers` that runs the held artifact proven on the
/// same system and processor ([`UpdateRecord::peer_lift`], once per artifact). Returns whether
/// one did.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn lift_by(record: &mut UpdateRecord, peers: &[UpdateRecord], now: u64) -> bool {
    peers.iter().any(|peer| record.peer_lift(peer, now))
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "lab_tests.rs"]
mod tests;
