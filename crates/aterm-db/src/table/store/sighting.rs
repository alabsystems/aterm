// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A table store's revision read without opening the store (tracker LT-25C): a reader that holds
//! a [`Sighting`] of the store learns that the store stands as the sighting's open read it, and so
//! that its revision ([`Store::revision`]) has not moved, without authenticating a record.
//!
//! An observing open of a primary store holding no conflict certificate is a fixed function of
//! what it reads: the enrollment claim, judged against the session and its data folder; the
//! floor and group permit files beside the claim; the journal's checkpoint-attested prefix,
//! every byte of which a read of the journal hashes to the checkpoint (an open takes that read
//! from what this process keeps of the journal while its folder and files stand, by their
//! `lstat`, as the read saw them, and reads again once in so many such asks: `super::journals`);
//! and, of the session, its writer, its access observation and the verdict of each class the
//! records were classified under ([`crate::table::Verdict`]: whether the class's source is
//! discarded, and whether a historical writer grant and key open its payloads). A record's
//! signature, receipt, header, payload state and cutoff flag follow from its bytes and its
//! class's verdict; the quotas and the causal index from the records and the floor.
//! `super::kept` stands on the same split. A revision names those files' bytes too ([`Files`]),
//! and under a floor the cold payloads' file, from which a read below the floor takes its
//! payloads (`super::cold`); a store on no floor takes none from it, and neither a revision nor
//! a sighting of it reads that file (`Store::takes_cold`).
//!
//! So a sighting keeps, from one observing open that succeeded
//! ([`Store::observe_if_exists_sighted`], then [`Store::sighted_revision`]), the revision it read
//! and every one of those inputs: the claim's bytes, the floor's and the group permit's, the
//! journal's acknowledged prefix (its file, length and SHA-256), under a floor the cold file's
//! SHA-256, each class's verdict and the session's access observation; the claim names the
//! table, this Mac's writer and its enrollment, and an open judges its hold on the data folder.
//! [`Store::sighting_stands`] makes every judgment an open makes before its journal
//! ([`Store::claimed`]: the session's source reads and context, the location, the claim read and
//! judged and its hold on the data folder), then compares each input, the cheapest first: the
//! access observation; the claim, floor and permit byte for byte; each class's verdict asked
//! again in one read batch, with the batch's fences ([`crate::table::Verdict::same`]); under a
//! floor the cold file hashed again; the journal read handed the kept prefix
//! ([`aterm_journal::read_committed_opened`]), which must answer that the same file still attests
//! exactly that prefix, having hashed every acknowledged byte again; and last the store's own
//! check ([`super::Custody`]): the session current for the table, and the claim read again
//! unchanged. That check compares no actor or access observation of its own: the claim's bytes
//! name this Mac's writer, and the access observation, fixed for a session's life, was compared
//! first. Only where every one stands is the answer that the store is unchanged; anything that
//! differs, and any refusal, is the answer that the reader must open the store, whose own
//! judgment, refusal included, is then the answer. What a full open verifies is unchanged: the
//! open that records a sighting's verdicts asks them beside its replay's read batch, whose keys
//! and closing checks are those of the open that records none
//! (`crate::table::ReadBatch::class_verdict_aside`). A sighting hashes the journal's bytes every
//! time, so after a fault that moved no stamp it fails while the open the reader falls back to
//! may still be answered from what this process keeps: that open reads the same revision until
//! its next read of the bytes, and each page until then costs a sighting that failed and that
//! open.
//!
//! A sighting is kept only in the reader's memory, so nothing on disk can forge one, and none is
//! made of a writer's store, a recovery store, a combined or displaced view, or a store that
//! holds a conflict certificate, whose judgments are not a class's.
//!
//! What it costs is a read and a SHA-256 of the journal's acknowledged prefix and, under a
//! floor, of the cold file, where an open also checks every record's signature and opens
//! payloads the process does not keep a fact for: at 105,043 records (68 MB of journal, 21.6 MB of cold payloads) about
//! 124 ms for the journal, hashed by the journal's own SHA-256 as every open hashes it, and
//! 7 ms for the cold file, hashed with the CPU's SHA-256 instructions ([`cold::digest`]),
//! against an open's 1.86 s.

use super::{
    ANCHOR_LEN, Capacity, Claimed, Custody, Error, Region, Result, Scope, Session, Store, cold,
    floor, group, kept,
};
use astream_wire::sync::Digest;
use aterm_journal::{Committed, Prefix, Retain};
use std::path::Path;

/// The journal's name in a table's data folder.
const JOURNAL: &str = "table";

/// The store's files beside its claim and journal as an observing open read them
/// (`Store::open_claimed_classified`): what a revision names of them besides the claim and,
/// under a floor, the cold file.
#[derive(Clone)]
pub(super) struct Files {
    /// The journal's acknowledged prefix the open's records were identified from: its file,
    /// length and the SHA-256 of every byte of it, which the checkpoint attests.
    pub(super) journal: Prefix,
    /// The floor file's bytes, where one stands.
    pub(super) floor: Option<Vec<u8>>,
    /// The group permit file's bytes, where one stands.
    pub(super) group: Option<Vec<u8>>,
}

impl Files {
    /// Hash into a revision these files, the `claim` read beside them and the `cold` file's
    /// SHA-256, where a read takes payloads from that file and one is there: every byte of the
    /// claim, the floor and the group permit, and of the journal its acknowledged length and
    /// SHA-256, which name every byte of that prefix.
    pub(super) fn digest_into(
        &self,
        claim: &[u8],
        cold: Option<&Digest>,
        hash: &mut aterm_digest::Sha256,
    ) {
        fn optional(hash: &mut aterm_digest::Sha256, bytes: Option<&[u8]>) {
            match bytes {
                Some(bytes) => {
                    hash.update([1]);
                    hash.update((bytes.len() as u64).to_le_bytes());
                    hash.update(bytes);
                }
                None => hash.update([0]),
            }
        }
        hash.update((claim.len() as u64).to_le_bytes());
        hash.update(claim);
        optional(hash, self.floor.as_deref());
        optional(hash, self.group.as_deref());
        let acknowledged = self.journal.acknowledged();
        hash.update(acknowledged.length.to_le_bytes());
        hash.update(acknowledged.digest);
        optional(hash, cold.map(<[u8; 32]>::as_slice));
    }
}

/// One observing open's revision of a primary store, with every input that open's result is a
/// function of (the module's doc): what a reader holds to learn, without opening the store again,
/// that the store stands as that open read it ([`Store::sighting_stands`]).
///
/// It names its store by the claim alone: a claim holds its table's scope, this Mac's writer and
/// its enrollment anchor, and an open judges its hold on the data folder, so a sighting asked of
/// another table, home or writer meets another claim.
pub(crate) struct Sighting {
    revision: Digest,
    observation: [u8; 64],
    claim: Vec<u8>,
    files: Files,
    /// The cold file's SHA-256, where the open stood on a floor and a file was there.
    cold: Option<Digest>,
    classes: kept::Classes,
}

impl Sighting {
    /// The revision the open read ([`Store::revision`]): what a reader that finds the store
    /// standing as this sighting's open read it answers (`crate::att::history`).
    pub(crate) const fn revision(&self) -> Digest {
        self.revision
    }
}

impl std::fmt::Debug for Sighting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sighting")
            .field("revision", &aterm_codec::hex::encode_lower(&self.revision))
            .field("classes", &self.classes.len())
            .finish_non_exhaustive()
    }
}

impl Store {
    /// [`Self::observe_if_exists_region`] in the primary region, recording the verdict of each
    /// class the store's records are classified under in the replay's own read batch, of which
    /// [`Self::sighted_revision`] makes a [`Sighting`]. The open is the same open: every record is
    /// authenticated and judged as there, and each verdict is asked beside the batch, leaving its
    /// keys as they were ([`kept::Recorded::aside`]), so the batch's closing checks again the keys
    /// that open's does, none more.
    ///
    /// # Errors
    /// As [`Self::observe_if_exists_region`].
    pub(crate) fn observe_if_exists_sighted(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
    ) -> Result<Option<Self>> {
        if Self::unenrolled(session, dir, scope, Region::Primary)? {
            return Ok(None);
        }
        let mut recorded = kept::Recorded {
            aside: true,
            ..kept::Recorded::default()
        };
        let opened = Self::claimed(
            session,
            dir,
            scope,
            Capacity::default(),
            true,
            Region::Primary,
        )
        .and_then(|claimed| {
            Self::open_claimed_classified(session, dir, scope, false, claimed, Some(&mut recorded))
        });
        match opened {
            Ok(mut store) => {
                store.classified = Some(recorded.classes);
                Ok(Some(store))
            }
            Err(error) if error.is_enrollment_pending() => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// This store's revision ([`Self::revision`]), and where a later read can learn it unchanged
    /// without opening the store, the [`Sighting`] that read holds: none for a store not opened
    /// by [`Self::observe_if_exists_sighted`], a writer's store, a recovery store, a combined or
    /// displaced view, one whose append is uncertain, or one that holds a conflict certificate.
    ///
    /// # Errors
    /// As [`Self::revision`].
    pub(crate) fn sighted_revision(
        &self,
        session: &Session<'_>,
    ) -> Result<(Digest, Option<Sighting>)> {
        let (revision, cold) = self.revision_read(session)?;
        let sighting = match (&self.classified, &self.files) {
            (Some(classes), Some(files))
                if self.region == Region::Primary
                    && self.log.is_none()
                    && !self.poisoned
                    && self.observed_primary.is_none()
                    && self.displaced.is_none()
                    && self.conflicts.is_empty() =>
            {
                Some(Sighting {
                    revision,
                    observation: self.observation,
                    claim: self.claim.clone(),
                    files: files.clone(),
                    cold,
                    classes: classes.clone(),
                })
            }
            _ => None,
        };
        Ok((revision, sighting))
    }

    /// Whether the primary store of `scope` at `dir` stands in `session` as `sighting`'s open
    /// read it, so that an observing open now would read the revision it read (the module's doc):
    /// every judgment an open makes before its journal, then each input compared. `false` where
    /// anything differs or any step refuses; the caller then opens the store, whose answer, a
    /// refusal included, stands.
    pub(crate) fn sighting_stands(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
        sighting: &Sighting,
    ) -> bool {
        #[cfg(test)]
        SIGHTINGS_ASKED.set(SIGHTINGS_ASKED.get() + 1);
        Self::sighted_now(session, dir, scope, sighting).unwrap_or(false)
    }

    /// [`Self::sighting_stands`], with a refusal as it met it.
    fn sighted_now(
        session: &Session<'_>,
        dir: &Path,
        scope: Scope,
        sighting: &Sighting,
    ) -> Result<bool> {
        if session.access_observation() != sighting.observation {
            return Ok(false);
        }
        let Claimed {
            sources,
            claim_dir,
            claim_name,
            claim,
            ..
        } = Self::claimed(
            session,
            dir,
            scope,
            Capacity::default(),
            true,
            Region::Primary,
        )?;
        if claim != sighting.claim {
            return Ok(false);
        }
        if floor::bytes(&claim_dir, &claim_name)? != sighting.files.floor {
            return Ok(false);
        }
        // The cheap comparisons first, the journal's hash, most of the cost, last: a change
        // caught by any other input costs no journal read.
        if group::bytes(&claim_dir, &claim_name)? != sighting.files.group {
            return Ok(false);
        }
        let verdicts = sources.with_read_batch(scope, |batch| {
            for (class, verdict) in &sighting.classes {
                if !batch.class_verdict(class)?.same(verdict) {
                    return Ok(false);
                }
            }
            Ok(true)
        })?;
        if !verdicts {
            return Ok(false);
        }
        let limits = Capacity::default().journal();
        // The cold file is named only where a read takes payloads from it (`Store::takes_cold`):
        // under a floor, which the sighting's open then stood on and, its bytes compared above,
        // an open now stands on too.
        if sighting.files.floor.is_some() && cold::digest(dir, limits.bytes)? != sighting.cold {
            return Ok(false);
        }
        let (committed, _) = aterm_journal::read_committed_opened(
            dir,
            JOURNAL,
            &claim[..ANCHOR_LEN],
            limits,
            Some(&sighting.files.journal),
            Retain::Digest,
        )
        .map_err(Error::from)?;
        if !matches!(committed, Committed::Unchanged) {
            return Ok(false);
        }
        #[cfg(test)]
        if let Some(meanwhile) = BEFORE_CUSTODY.take() {
            meanwhile();
        }
        let observation = session.access_observation();
        let custody = Custody {
            scope,
            actor: session.actor(),
            observation: &observation,
            data_dir: dir,
            claim_dir: &claim_dir,
            claim_name: &claim_name,
            claim: &claim,
            displaced: None,
        };
        let stands =
            custody.admits(session)? && custody.current_claim()?.as_deref() == Some(&claim[..]);
        #[cfg(test)]
        if stands {
            SIGHTINGS_STOOD.set(SIGHTINGS_STOOD.get() + 1);
        }
        Ok(stands)
    }
}

#[cfg(test)]
thread_local! {
    /// Run once by this thread's next sighting asked to stand, once it compared every input and
    /// before its custody check ([`with_before_custody`]).
    static BEFORE_CUSTODY: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
    /// How many sightings this thread asked to stand ([`Store::sighting_stands`]).
    static SIGHTINGS_ASKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many of them stood.
    static SIGHTINGS_STOOD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `read`, with `meanwhile` run by this thread's next sighting asked to stand
/// ([`Store::sighting_stands`]) once it compared every input and before its custody check: what
/// changed while a sighting was judged.
#[cfg(test)]
pub(super) fn with_before_custody<T>(
    meanwhile: impl FnOnce() + 'static,
    read: impl FnOnce() -> T,
) -> T {
    BEFORE_CUSTODY.set(Some(Box::new(meanwhile)));
    let read = read();
    assert!(
        BEFORE_CUSTODY.take().is_none(),
        "no sighting reached its custody check"
    );
    read
}

/// How many sightings this thread asked to stand, and how many stood.
#[cfg(test)]
pub(crate) fn test_sightings() -> (usize, usize) {
    (SIGHTINGS_ASKED.get(), SIGHTINGS_STOOD.get())
}

#[cfg(test)]
pub(crate) use tests::permit_group;

#[cfg(test)]
#[path = "sighting_tests.rs"]
mod tests;
