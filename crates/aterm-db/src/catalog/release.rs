// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The storage release for sealed tables (docs/DESIGN-alab-lab-model-2026-11-01.md §3.8, §3.10,
//! step 13; model `DbStorageRelease`, `aterm_models_db::storage_release_model`).
//!
//! A sealed table keeps its storage directory, and so a slot of [`super::MAX_TABLES`], on every
//! Mac that ever held it: a seal only makes its declarations conflict, so nothing reads, writes
//! or repairs it again. This frees that storage, on each Mac by itself, only where all of these
//! hold:
//!
//! - **This build compiled the table's retirement** ([`retired`]). A conflict this build did
//!   not compile is never released: any Writer can declare a twin of any table, and the
//!   storage of a table someone else sealed is not this code's to free.
//! - **The catalog shows it sealed**: every declaration at its identity is the retired
//!   descriptor or its twin (the next version, [`Directory::seal`]), and the twin is there.
//! - **Its data is carried or has lapsed.** A table whose rows only count for a grace after the
//!   seal (the claims tables of an F19 build from before the ring: `claims::legacy_active`) is
//!   released only once [`LAPSE_S`] has passed by this Mac's own clock since it first saw the
//!   seal, and by the sealer's claim too. A table a roll restated into its next generation is
//!   released only once that generation's cover rows carry every record this Mac holds of it
//!   (`alab::roll::covered`): a record no restatement carried yet is data some peer may still
//!   need, so its storage stays, and the roll run on this Mac carries it.
//!
//! What is released is this Mac's primary and recovery storage directory of the table and its
//! enrollment claims; the catalog's declarations stay, so a new Mac still learns the table is
//! sealed. A pass cut short is finished by the next: the first-seen stamps it writes are durable
//! before anything is deleted, and a deleted directory is simply not there next time.

use super::{Descriptor, Directory, Entry, Snapshot, State};
use crate::{
    Error, Result,
    table::{
        Session,
        store::{Region, claim_dir, claim_name},
    },
};
use astream_wire::sync::Digest;
use aterm_secrets::commons::journal::Trust;
use aterm_uds::private_file::{self, PrivateDir, PrivateFileError};
use std::collections::BTreeMap;

/// How long after a seal a lapsing table's storage stays, by this Mac's clock from when it first
/// saw the seal and by the sealer's claim: a week, well past the two days an old build's claims
/// rows count (`claims::LEGACY_GRACE_S`).
pub(crate) const LAPSE_S: u64 = 7 * 86_400;
const _: () = assert!(LAPSE_S > crate::accounts::claims::LEGACY_GRACE_S);

/// How a retired table's data outlives its storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Carry {
    /// Its rows count only for a grace after the seal; then nothing reads them.
    Lapses { grace_s: u64 },
    /// A roll restated it into its next generation, whose cover rows say what was carried.
    Restated,
}

/// A table this build retires, and how its data outlives it.
#[derive(Clone, Debug)]
pub(crate) struct Retired {
    pub(crate) descriptor: Descriptor,
    pub(crate) carry: Carry,
}

/// Every table this build retires in `trust`'s lab: the pre-ring claims generations, which
/// lapse, and every registry generation but the last, which a roll restates.
pub(crate) fn retired(trust: Trust) -> Vec<Retired> {
    let mut out: Vec<Retired> = crate::accounts::claims::legacy_descriptors(trust)
        .into_iter()
        .map(|descriptor| Retired {
            descriptor,
            carry: Carry::Lapses { grace_s: LAPSE_S },
        })
        .collect();
    let registry = crate::att::REGISTRY.chain();
    for compiled in &registry[..registry.len() - 1] {
        if let Ok(descriptor) = compiled.descriptor(trust) {
            out.push(Retired {
                descriptor,
                carry: Carry::Restated,
            });
        }
    }
    out
}

/// When `descriptor` was sealed by the latest sealer's claim (Unix ms): `None` unless its catalog
/// `entry` holds only `descriptor` and its twin, the twin at least once, and is no longer
/// subscribed.
pub(crate) fn sealed_at(entry: Option<&Entry>, descriptor: &Descriptor) -> Option<u64> {
    let entry = entry?;
    descriptor.version.checked_add(1)?;
    if entry.state == State::Subscribed {
        return None;
    }
    let mut max_twin = None;
    for d in &entry.declarations {
        if d.descriptor.is_twin_of(descriptor) {
            max_twin = Some(max_twin.map_or(d.authored_ms, |m: u64| m.max(d.authored_ms)));
        } else if d.descriptor != *descriptor {
            return None;
        }
    }
    max_twin
}

/// Why a retired table's storage stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// The catalog does not show it sealed.
    NotSealed,
    /// Its rows may still count for a peer: the grace since the seal has not passed.
    Lapsing,
    /// This Mac holds records of it that no restatement carried yet.
    NotCarried,
}

impl Kept {
    /// The word reports use.
    pub const fn word(self) -> &'static str {
        match self {
            Self::NotSealed => "not-sealed",
            Self::Lapsing => "lapsing",
            Self::NotCarried => "not-carried",
        }
    }
}

/// One release pass's decision for one retired table this Mac stores, at `now_s` by its clock:
/// the first-seen stamp to keep (`seen_s`, set to `now_s` at the first pass that sees the seal)
/// and whether the storage goes. `sealed_ms` is [`sealed_at`]'s; `covered` is whether the next
/// generation's cover rows carry every record this Mac holds of a restated table.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbStorageRelease",
        action = "Pass",
        project = "aterm_db::catalog::release::tests::release_transition"
    )
)]
pub(crate) fn pass(
    sealed_ms: Option<u64>,
    carry: Carry,
    seen_s: Option<u64>,
    now_s: u64,
    covered: bool,
) -> (Option<u64>, std::result::Result<(), Kept>) {
    let Some(sealed_ms) = sealed_ms else {
        return (seen_s, Err(Kept::NotSealed));
    };
    let seen_s = seen_s.unwrap_or(now_s);
    let decision = match carry {
        Carry::Lapses { grace_s } => {
            let mine = now_s.saturating_sub(seen_s) >= grace_s;
            let theirs = now_s.saturating_sub(sealed_ms / 1000) >= grace_s;
            if mine && theirs {
                Ok(())
            } else {
                Err(Kept::Lapsing)
            }
        }
        Carry::Restated if covered => Ok(()),
        Carry::Restated => Err(Kept::NotCarried),
    };
    (Some(seen_s), decision)
}

/// What one release pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Released {
    /// The tables whose storage this pass deleted.
    pub released: Vec<Digest>,
    /// The retired tables this Mac still stores, and why each stays.
    pub kept: Vec<(Digest, Kept)>,
}

/// How often a claims write runs the release pass at most ([`Directory::release_due`]): a
/// restated table's `covered` answer reads both registry generations whole, which a claims write
/// must not pay each time while a Mac holds records no cover carries. A day is well inside
/// [`LAPSE_S`], and the roll runs its own pass at once.
pub(crate) const RELEASE_EVERY_S: u64 = 86_400;
const _: () = assert!(RELEASE_EVERY_S < LAPSE_S);

/// The file in the claim directory that keeps when this Mac last began a due release pass.
const PASSED: &str = "release-pass";

/// The file in the claim directory that keeps when this Mac first saw each retired table sealed.
const SEEN: &str = "release-seen";
/// Its largest size: a line per retired table.
const SEEN_MAX: usize = 4096;

fn read_seen(dir: &std::path::Path) -> Result<BTreeMap<Digest, u64>> {
    let bytes = match private_file::read_private(dir, SEEN, SEEN_MAX) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(BTreeMap::new()),
        Err(_) => return Err(Error::new("the storage release's stamps are unreadable")),
    };
    let damaged = || Error::new("the storage release's stamps are damaged");
    let text = std::str::from_utf8(&bytes).map_err(|_| damaged())?;
    let mut seen = BTreeMap::new();
    for line in text.lines() {
        let (id, at) = line.split_once(' ').ok_or_else(damaged)?;
        let id = crate::database::decode_hex::<32>(id).map_err(|_| damaged())?;
        seen.insert(id, at.parse().map_err(|_| damaged())?);
    }
    Ok(seen)
}

fn write_seen(dir: &std::path::Path, seen: &BTreeMap<Digest, u64>) -> Result<()> {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(seen.len() * 86);
    for (id, at) in seen {
        let _ = writeln!(text, "{} {at}", aterm_codec::hex::encode_lower(id));
    }
    aterm_journal::create_private_dir_durable(dir)?;
    aterm_journal::write_private_durable(dir, SEEN, text.as_bytes())?;
    Ok(())
}

/// Delete a private storage directory, if there is one.
fn remove_storage(path: &std::path::Path) -> Result<()> {
    match private_file::judge_private_dir(path)? {
        PrivateDir::Missing => Ok(()),
        PrivateDir::Private => Ok(std::fs::remove_dir_all(path)?),
        PrivateDir::Open(_) => Err(Error::new("sealed table storage is not private")),
    }
}

fn remove_claim(dir: &std::path::Path, name: &str) -> Result<()> {
    match std::fs::remove_file(dir.join(name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Whether a due pass runs at `now_s` after the last began at `last_s`: once
/// [`RELEASE_EVERY_S`] has passed, or when this Mac's clock went back past the stamp.
pub(crate) const fn due(last_s: Option<u64>, now_s: u64) -> bool {
    match last_s {
        None => true,
        Some(last_s) => now_s < last_s || now_s - last_s >= RELEASE_EVERY_S,
    }
}

impl Directory {
    /// [`Self::release_sealed`] at most once per [`RELEASE_EVERY_S`] by this Mac's clock, as a
    /// claims write runs it: `None` when no pass was due. The stamp is durable before the pass
    /// runs, so a pass that fails or is cut short is not retried on every write either.
    ///
    /// # Errors
    /// As [`Self::release_sealed`], or the stamp cannot be read or written.
    pub(crate) fn release_due(
        &self,
        session: &Session<'_>,
        snapshot: &Snapshot,
        now_s: u64,
        covered: &dyn Fn(&Session<'_>, &Directory, &Retired) -> Result<bool>,
    ) -> Result<Option<Released>> {
        if !session.has_control_writer() {
            return Err(Error::new(
                "the storage release requires a locked control observation",
            ));
        }
        let claims = claim_dir(session)?;
        let last = match private_file::read_private(&claims, PASSED, 32) {
            Ok(bytes) => std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| text.trim().parse().ok()),
            Err(PrivateFileError::Missing) => None,
            Err(_) => return Err(Error::new("the storage release's pass stamp is unreadable")),
        };
        if !due(last, now_s) {
            return Ok(None);
        }
        aterm_journal::create_private_dir_durable(&claims)?;
        aterm_journal::write_private_durable(&claims, PASSED, now_s.to_string().as_bytes())?;
        self.release_sealed(session, snapshot, now_s, covered)
            .map(Some)
    }

    /// Whether this Mac stores anything of `scope`: a primary or recovery directory, or an
    /// enrollment claim.
    fn stores(&self, session: &Session<'_>, scope: crate::table::Scope) -> Result<bool> {
        let name = claim_name(scope);
        for region in [Region::Primary, Region::Recovery] {
            if region.claims(session)?.join(&name).try_exists()? {
                return Ok(true);
            }
        }
        Ok(self.primary().path(scope)?.try_exists()?
            || self.recovery().path(scope)?.try_exists()?)
    }

    /// Release this Mac's storage of each retired table ([`retired`]) that `snapshot` shows
    /// sealed and whose data is carried or has lapsed ([`pass`]), under the control writer, at
    /// `now_s` by this Mac's clock. `covered` answers, for a restated table, whether the next
    /// generation's cover rows carry every record this Mac holds of it; it is asked only for a
    /// sealed table this Mac stores.
    ///
    /// # Errors
    /// No control writer, unreadable or unwritable stamps, `covered` fails, or storage that is
    /// not private or cannot be deleted.
    pub(crate) fn release_sealed(
        &self,
        session: &Session<'_>,
        snapshot: &Snapshot,
        now_s: u64,
        covered: &dyn Fn(&Session<'_>, &Directory, &Retired) -> Result<bool>,
    ) -> Result<Released> {
        if !session.has_control_writer() {
            return Err(Error::new(
                "the storage release requires a locked control observation",
            ));
        }
        let claims = claim_dir(session)?;
        let mut seen = read_seen(&claims)?;
        let mut seen_changed = false;
        let mut doomed = Vec::new();
        let mut released = Released::default();
        for table in retired(self.trust) {
            let scope = table.descriptor.scope;
            let id = scope.digest();
            if scope == self.catalog_scope() || !self.stores(session, scope)? {
                seen_changed |= seen.remove(&id).is_some();
                continue;
            }
            let sealed_ms = sealed_at(snapshot.entries.get(&id), &table.descriptor);
            let carried = match (sealed_ms, table.carry) {
                (Some(_), Carry::Restated) => covered(session, self, &table)?,
                _ => false,
            };
            let (stamp, decision) = pass(
                sealed_ms,
                table.carry,
                seen.get(&id).copied(),
                now_s,
                carried,
            );
            seen_changed |= match stamp {
                Some(at) => seen.insert(id, at) != Some(at),
                None => seen.remove(&id).is_some(),
            };
            match decision {
                Ok(()) => doomed.push((id, scope)),
                Err(kept) => released.kept.push((id, kept)),
            }
        }
        // The stamps are durable before anything goes, so a pass cut short never restarts a
        // grace it already counted.
        if seen_changed {
            write_seen(&claims, &seen)?;
        }
        if doomed.is_empty() {
            return Ok(released);
        }
        let recovery_claims = Region::Recovery.claims(session)?;
        for &(id, scope) in &doomed {
            remove_storage(&self.primary().path(scope)?)?;
            remove_storage(&self.recovery().path(scope)?)?;
            let name = claim_name(scope);
            for dir in [&claims, &recovery_claims] {
                remove_claim(dir, &name)?;
            }
            released.released.push(id);
        }
        for dir in [
            &self.primary().root,
            &self.recovery().root,
            &claims,
            &recovery_claims,
        ] {
            if dir.try_exists()? {
                aterm_durable::durable_dir(dir)?;
            }
        }
        for id in &released.released {
            seen.remove(id);
        }
        write_seen(&claims, &seen)?;
        Ok(released)
    }
}

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbStorageRelease",
        action = "Lapses",
        reason = "The conformance walk chooses a lapsing retirement (the pre-ring claims tables); the compiled retirement list, not a production step, says which a table is. release_transition and a_lapsed_claims_table_is_released_only_after_its_grace exercise it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbStorageRelease",
        action = "Restated",
        reason = "The conformance walk chooses a restated retirement (a registry generation); the compiled retirement list says which a table is. release_transition and the roll tests exercise it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbStorageRelease",
        action = "Restate",
        reason = "A roll's restatement (alab::roll) is the environment of the release decision; the walk sets what its cover carried. The roll tests run the restatement itself."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbStorageRelease",
        action = "RestateLate",
        reason = "The roll run again on a Mac that holds late records; the walk sets the cover it writes. a_late_edit_is_carried_before_its_mac_releases runs it on Commons."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbStorageRelease",
        action = "Seal",
        reason = "Directory::seal's declaration of the twin is a catalog write the walk stands for with a sealed entry; the Commons tests seal through it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbStorageRelease",
        action = "SealSkewed",
        reason = "A sealer whose clock claims the seal is old is an environment the walk sets through the declaration's authored time."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbStorageRelease",
        action = "Late",
        reason = "A record arriving that no restatement carried is a repair round's delivery; the walk raises this Mac's held frontier, and the Commons partition test delivers one."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbStorageRelease",
        action = "Elapse",
        reason = "Time passing is the environment: the walk moves this Mac's clock and the sealer's claim."
    )
)]
#[doc(hidden)]
pub fn db_storage_release_environment() {}

#[cfg(test)]
pub(crate) mod tests;
