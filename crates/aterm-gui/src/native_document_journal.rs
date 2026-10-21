// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable private crash journals for native documents.
//!
//! A journal filename is derived from the canonical URI but never contains it:
//! it is the document's [`JournalDocumentKey`], a MAC of the URI under the
//! per-install secret kept beside the journals ([`load_or_mint_journal_secret`]).
//! Journals named by the older unkeyed identity are read once, to migrate them
//! ([`LegacyJournal`]).
//! Every publication uses a same-directory temporary file, `sync_all`, atomic
//! rename, and directory sync. Thus an interrupted append leaves the previous
//! complete image recoverable instead of exposing a valid-prefix ambiguity.
//! Existing images and lock files are admitted through non-blocking, no-follow,
//! regular-file handles so a planted FIFO, device, or link cannot park Manual or
//! redirect persistence outside the private journal directory.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_buffer::Seq;

use crate::document_store::{DocumentId, DocumentSnapshot};
use crate::durable::Flush;
use crate::native_document_host::{replace_file, sync_directory};
use crate::native_document_io::{
    ContentFingerprint, DraftJournalReducer, DurableCheckpoint, DurableSource, JOURNAL_SECRET_LEN,
    JournalAppendPlan, JournalAppendProof, JournalAppendResult, JournalDocumentKey, JournalEdit,
    JournalError, JournalImageGeneration, JournalOwnerId, JournalRecord, JournalReduction,
    JournalSecret, JournalStage, LegacyJournal, MAX_INSERT_BYTES, MAX_JOURNAL_BYTES,
    StoredJournalRecovery, compact_journal, encode_journal, recover_journal_for,
};

#[cfg(any(unix, windows))]
#[path = "native_document_journal_owner.rs"]
mod owner;

/// Where no image can carry an owner set (neither an extended attribute nor an
/// alternate data stream), nothing is marked and nothing is taken: every
/// journal stays, as it always did there.
#[cfg(not(any(unix, windows)))]
mod owner {
    use std::fs::File;
    use std::path::{Path, PathBuf};

    use super::{ContentFingerprint, JournalDocumentKey, JournalImageGeneration, JournalOwnerId};

    #[derive(Debug)]
    pub(crate) enum JournalOwner {}

    impl JournalOwner {
        pub(crate) fn id(&self) -> JournalOwnerId {
            match *self {}
        }
    }

    pub(super) enum Candidate {}

    pub(crate) fn establish(_root: &Path) -> Result<JournalOwner, String> {
        Err("journal owners need extended attributes or streams".to_string())
    }

    pub(super) fn names_file(_path: &Path, _file: &File) -> bool {
        true
    }

    pub(super) fn mark_publication(
        _temporary: &File,
        _temporary_path: &Path,
        _path: &Path,
        _expected: JournalImageGeneration,
        _bytes: ContentFingerprint,
        _owner: Option<JournalOwnerId>,
    ) -> Vec<u8> {
        Vec::new()
    }

    pub(super) fn confirm_publication(_path: &Path, _mark: &[u8]) {}

    pub(super) fn adopt(_path: &Path, _image: ContentFingerprint, _owner: Option<JournalOwnerId>) {}

    pub(super) fn released(_path: PathBuf, _key: JournalDocumentKey) -> Option<Candidate> {
        None
    }

    pub(super) fn settle_releases(owner: JournalOwnerId, _released: Vec<Candidate>) {
        match owner {}
    }
}

const COMPACT_RECORDS: usize = 128;
const COMPACT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryNotice {
    Recovered,
    DiskConflict,
    /// The journal did not decode; its cause is logged where it is found.
    Corrupt,
}

impl RecoveryNotice {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Recovered => "Recovered an unsaved draft — the file on disk is unchanged",
            Self::DiskConflict => {
                "An unsaved draft conflicts with newer disk content; showing the file on disk (the draft is kept)"
            }
            Self::Corrupt => "An unsaved draft could not be read; showing the file on disk (it is kept)",
        }
        .to_string()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct JournalOpenDecision {
    pub(crate) key: JournalDocumentKey,
    pub(crate) path: PathBuf,
    pub(crate) recovered_text: Option<String>,
    pub(crate) notice: Option<RecoveryNotice>,
    preserve_existing: bool,
    expected_image: JournalImageGeneration,
    /// The keyed image the inspection read holds no draft ([`holds_no_draft`]):
    /// the open decides the same with or without it, so `initialize` publishes
    /// over its absence too — the launch sweep may take a clean image nobody
    /// holds yet between the two (`owner`).
    expected_clean: bool,
    /// The legacy journal this open read in place of the keyed one.
    legacy: Option<LegacyJournal>,
}

#[derive(Clone, Debug)]
pub(crate) struct InitializedJournal {
    pub(crate) key: JournalDocumentKey,
    pub(crate) path: PathBuf,
    pub(crate) durable_seq: Seq,
    pub(crate) durable_text: Arc<str>,
    pub(crate) image_fingerprint: ContentFingerprint,
    pub(crate) notice: Option<RecoveryNotice>,
    /// A legacy journal not yet retired: retired before the first write.
    legacy: Option<LegacyJournal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct JournalRewriteGeneration(pub(crate) u64);

#[derive(Clone, Debug)]
pub(crate) struct JournalRewritePlan {
    pub(crate) generation: JournalRewriteGeneration,
    pub(crate) key: JournalDocumentKey,
    pub(crate) base_durable: Seq,
    pub(crate) expected_image: ContentFingerprint,
    pub(crate) target_seq: Seq,
    pub(crate) target_text: Arc<str>,
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) fingerprint: ContentFingerprint,
    /// Retired before this rewrite publishes ([`LegacyJournal`]).
    pub(crate) retire_legacy: Option<LegacyJournal>,
    /// Named on the image it publishes ([`DraftJournalHost::owner_id`]).
    pub(crate) owner: Option<JournalOwnerId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JournalRewriteProof {
    pub(crate) bytes_len: usize,
    pub(crate) fingerprint: ContentFingerprint,
    pub(crate) file_synced: bool,
    pub(crate) renamed_over_journal: bool,
    pub(crate) directory_synced: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalRewriteResult {
    Committed(JournalRewriteProof),
    Failed(String),
}

impl JournalRewritePlan {
    pub(crate) fn verifies(&self, proof: JournalRewriteProof) -> bool {
        proof.bytes_len == self.bytes.len()
            && proof.fingerprint == self.fingerprint
            && proof.file_synced
            && proof.renamed_over_journal
            && proof.directory_synced
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DraftJournalHost {
    root: PathBuf,
    /// Every document key is a MAC under it ([`JournalDocumentKey::keyed`]).
    secret: JournalSecret,
    /// This store's owner identity, named on every image it publishes, so the
    /// launch sweep never takes one while this process lives (`owner`). `None`
    /// (no lease could be taken; never off unix) publishes unmarked images,
    /// which no sweep takes.
    owner: Option<Arc<owner::JournalOwner>>,
}

#[derive(Clone, Debug)]
struct SavedBaseline {
    seq: Seq,
    text: Arc<str>,
}

#[derive(Clone, Debug)]
struct InflightRewrite {
    plan: JournalRewritePlan,
    checkpoint: SavedBaseline,
}

#[derive(Clone, Debug)]
struct JournalEntry {
    key: JournalDocumentKey,
    path: PathBuf,
    reducer: DraftJournalReducer,
    durable_text: Arc<str>,
    durable_image: ContentFingerprint,
    desired: DocumentSnapshot,
    append_text: Option<Arc<str>>,
    next_rewrite_generation: u64,
    rewrite_inflight: Option<InflightRewrite>,
    pending_checkpoint: Option<SavedBaseline>,
    /// The first snapshot this store published — the file contents the draft
    /// was made against (the file at open, or the last verified save), or the
    /// draft itself after a re-seat found no baseline to copy — as a
    /// fingerprint and a sequence. A re-seat keeps it: the text itself is read
    /// back from the image on disk, never held twice in memory.
    baseline: ContentFingerprint,
    baseline_seq: Seq,
    /// Whether `baseline` is the file's contents: set by `initialize` and a
    /// verified checkpoint, cleared by a re-seat that records its target as its
    /// own baseline, twice. Only while it holds may a re-seat publish a
    /// lone snapshot, which every later open reads as clean
    /// ([`holds_no_draft`]).
    baseline_is_file: bool,
    /// The image on disk may no longer be this store's own. An update's
    /// successor republishes the journal of every document it restores, under
    /// ITS sequence numbers; when that update rolls back, this process owns the
    /// document again and must re-establish the image before it appends.
    reseat_owed: bool,
    reseat_inflight: Option<JournalReseatPlan>,
    /// The last re-seat failed. It is retried at every drive, but a draft
    /// whose journal keeps refusing is said out loud, not waited on in silence
    /// ([`DraftCarryRefusal::ReseatFailed`]).
    reseat_failed: bool,
    /// The last append failed, and nothing has made the head durable since.
    /// The update retries it itself before it asks
    /// (`App::drive_owed_document_journals`), so the retry is on the worker
    /// whenever the update reads this draft: without this fact a journal that
    /// keeps refusing reads as one that is merely busy, and the update waits
    /// on it in silence for good ([`DraftCarryRefusal::AppendFailed`]).
    append_failed: bool,
    /// A legacy journal this document was migrated from and that is not yet
    /// retired: bound into every write plan, which retires it before it
    /// publishes, and cleared once one lands ([`LegacyJournal`]).
    legacy: Option<LegacyJournal>,
}

#[derive(Clone, Debug)]
pub(crate) enum JournalEffect {
    Append {
        path: PathBuf,
        key: JournalDocumentKey,
        plan: JournalAppendPlan,
    },
    Rewrite {
        path: PathBuf,
        plan: JournalRewritePlan,
    },
    Reseat {
        path: PathBuf,
        plan: JournalReseatPlan,
    },
}

/// Re-establish this process's journal after an update's successor wrote over
/// it (see [`JournalEntry::reseat_owed`]). The image is composed on the worker,
/// under the journal lock, from what is actually on disk: the baseline snapshot
/// is taken from the image found there when it is still this store's baseline,
/// and anything found there that this store never held is preserved aside first.
#[derive(Clone, Debug)]
pub(crate) struct JournalReseatPlan {
    pub(crate) generation: JournalRewriteGeneration,
    pub(crate) document: DocumentId,
    pub(crate) key: JournalDocumentKey,
    /// The image this store last published. Still on disk means nothing to do.
    pub(crate) ours: ContentFingerprint,
    pub(crate) baseline: ContentFingerprint,
    pub(crate) baseline_seq: Seq,
    /// [`JournalEntry::baseline_is_file`].
    pub(crate) baseline_is_file: bool,
    /// What this store last proved durable, and what it holds now: an image
    /// that recovers to either is one this store already had.
    pub(crate) durable_text: Arc<str>,
    pub(crate) target_seq: Seq,
    pub(crate) target_text: Arc<str>,
    /// Retired before this re-seat publishes ([`LegacyJournal`]).
    pub(crate) retire_legacy: Option<LegacyJournal>,
    /// Named on the image it publishes, or joined to the one it finds
    /// ([`DraftJournalHost::owner_id`]).
    pub(crate) owner: Option<JournalOwnerId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalReseatResult {
    /// The image on disk is still the one this store published.
    Unchanged,
    /// A fresh image recovering to the plan's target was published. `baseline`
    /// is its first snapshot: the plan's own when the image found on disk still
    /// carried it or the target is that baseline, else the target itself (the
    /// file's baseline was not there to copy, so a later open reports a disk
    /// conflict rather than replaying the draft over contents it was not made
    /// against, and keeps it aside rather than replacing it).
    /// `baseline_is_file` says whether that snapshot is still the file's
    /// contents, `preserved` whether the image found there was set aside first.
    Republished {
        image: ContentFingerprint,
        baseline: ContentFingerprint,
        baseline_seq: Seq,
        baseline_is_file: bool,
        preserved: bool,
    },
    Failed(String),
}

/// Why a document's unsaved draft cannot be carried through an update right
/// now, read from this store's memory alone (the disk half is
/// [`verify_carried_draft`]). `Writing` and `Reseating` clear by themselves;
/// the other two need something to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftCarryRefusal {
    /// The document has no journal in this process.
    Unjournaled,
    /// An append, checkpoint or re-seat is on the worker now.
    Writing,
    /// A re-seat is owed and has not run yet.
    Reseating,
    /// The last re-seat failed (it is retried, but not in silence).
    ReseatFailed,
    /// The last append failed and no write has landed since (it is retried,
    /// but not in silence).
    AppendFailed,
    /// The latest edit is not durable, nothing is writing it and no write of
    /// it failed: it was never planned (the drive itself refused, or the queue
    /// was full and the retry is waiting for a slot).
    Behind,
}

/// What this store vouches for about one document's head, for
/// [`verify_carried_draft`] to check against the image on disk.
#[derive(Clone, Debug)]
pub(crate) struct DraftCarryCandidate {
    pub(crate) path: PathBuf,
    pub(crate) key: JournalDocumentKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalCompletion {
    Durable {
        document: DocumentId,
        seq: Seq,
    },
    Failed {
        document: DocumentId,
        message: String,
    },
    Stale,
}

/// UI-thread owner for latest-wins serialization. At most one append or rewrite
/// exists per document. Edits arriving during I/O replace `desired`; after the
/// exact proof reduces, the next effect catches up directly to that latest head.
pub(crate) struct DocumentJournalStore {
    host: DraftJournalHost,
    entries: BTreeMap<DocumentId, JournalEntry>,
    /// See [`Self::set_reseat_fenced`].
    reseat_fenced: bool,
}

impl DocumentJournalStore {
    pub(crate) fn system_default() -> Result<Self, String> {
        Ok(Self {
            host: DraftJournalHost::system_default()?,
            entries: BTreeMap::new(),
            reseat_fenced: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(root: PathBuf) -> Result<Self, String> {
        Ok(Self {
            host: DraftJournalHost::new(root)?,
            entries: BTreeMap::new(),
            reseat_fenced: false,
        })
    }

    pub(crate) fn inspect_open(
        &self,
        canonical_uri: &str,
        disk_bytes: &[u8],
    ) -> Result<JournalOpenDecision, String> {
        self.host.inspect_open(canonical_uri, disk_bytes)
    }

    pub(crate) fn initialize(
        &mut self,
        decision: JournalOpenDecision,
        disk: &DocumentSnapshot,
        current: &DocumentSnapshot,
    ) -> Result<InitializedJournal, String> {
        let initialized = self.host.initialize(decision, disk, current)?;
        self.entries.insert(
            current.id,
            JournalEntry {
                key: initialized.key,
                path: initialized.path.clone(),
                reducer: DraftJournalReducer::new_with_key(
                    current.id,
                    initialized.key,
                    initialized.durable_seq,
                ),
                durable_text: initialized.durable_text.clone(),
                durable_image: initialized.image_fingerprint,
                desired: current.clone(),
                append_text: None,
                next_rewrite_generation: 1,
                rewrite_inflight: None,
                pending_checkpoint: None,
                baseline: ContentFingerprint::of(disk.text.as_bytes()),
                baseline_seq: disk.seq,
                baseline_is_file: true,
                reseat_owed: false,
                reseat_inflight: None,
                reseat_failed: false,
                append_failed: false,
                legacy: initialized.legacy.clone(),
            },
        );
        Ok(initialized)
    }

    /// Owe a re-seat on every journal this store writes (see
    /// [`JournalEntry::reseat_owed`]) and name the documents, so the host can
    /// drive each one. The worker finds most images untouched and does nothing.
    #[cfg(any(unix, test))]
    pub(crate) fn owe_reseat_all(&mut self) -> Vec<DocumentId> {
        self.entries
            .iter_mut()
            .map(|(document, entry)| {
                entry.reseat_owed = true;
                *document
            })
            .collect()
    }

    /// Whether any journal write — an append, a checkpoint, a re-seat — is on
    /// the worker now. An update may not start or commit over one: landing
    /// after the successor read the image it restores from, it would publish
    /// over (or onto) that image, and landing after Commit it would change the
    /// image under the successor's own journal store.
    pub(crate) fn writes_in_flight(&self) -> bool {
        self.entries.values().any(entry_busy)
    }

    pub(crate) fn reseat_owed(&self, document: DocumentId) -> bool {
        self.entries
            .get(&document)
            .is_some_and(|entry| entry.reseat_owed || entry.reseat_inflight.is_some())
    }

    /// Whether `document`'s last append or re-seat failed and nothing has
    /// landed since ([`JournalEntry::append_failed`],
    /// [`JournalEntry::reseat_failed`]).
    pub(crate) fn write_failed(&self, document: DocumentId) -> bool {
        self.entries.get(&document).is_some_and(|entry| {
            (entry.append_failed && entry.desired.seq > entry.reducer.durable_seq())
                || (entry.reseat_failed && (entry.reseat_owed || entry.reseat_inflight.is_some()))
        })
    }

    /// Documents whose journal owes work nothing is doing: a re-seat, or a
    /// head the last (failed) append never made durable. The host drives each
    /// before it asks whether a draft can be carried, so a transient failure
    /// is retried instead of holding the update until the next keystroke.
    pub(crate) fn owed_documents(&self) -> Vec<DocumentId> {
        self.entries
            .iter()
            .filter(|(_, entry)| {
                !entry_busy(entry)
                    && (entry.reseat_owed || entry.desired.seq > entry.reducer.durable_seq())
            })
            .map(|(document, _)| *document)
            .collect()
    }

    /// The memory half of carrying `head` through an update: this store has
    /// proved `head` durable — its latest append or checkpoint reduced at
    /// exactly this sequence — and nothing is writing, owed or failed. The disk
    /// half, what the image actually recovers to, is [`verify_carried_draft`].
    pub(crate) fn carry_candidate(
        &self,
        head: &DocumentSnapshot,
    ) -> Result<DraftCarryCandidate, DraftCarryRefusal> {
        let entry = self
            .entries
            .get(&head.id)
            .ok_or(DraftCarryRefusal::Unjournaled)?;
        if entry.reseat_failed && (entry.reseat_owed || entry.reseat_inflight.is_some()) {
            // Checked before "writing": the retry is usually on the worker when
            // the update asks, and a journal that keeps refusing must not read
            // as one that is merely busy.
            return Err(DraftCarryRefusal::ReseatFailed);
        }
        if entry.append_failed && entry.desired.seq > entry.reducer.durable_seq() {
            // Before "writing" for the same reason: the update drives the retry
            // of a failed append the moment before it asks, so a journal that
            // keeps refusing is always on the worker when asked.
            return Err(DraftCarryRefusal::AppendFailed);
        }
        if entry_busy(entry) {
            return Err(DraftCarryRefusal::Writing);
        }
        if entry.reseat_owed {
            return Err(DraftCarryRefusal::Reseating);
        }
        if entry.pending_checkpoint.is_some() {
            // A verified save is queued to prune the journal: the next drive
            // writes it.
            return Err(DraftCarryRefusal::Writing);
        }
        if entry.desired.seq != head.seq || entry.reducer.durable_seq() != head.seq {
            return Err(DraftCarryRefusal::Behind);
        }
        Ok(DraftCarryCandidate {
            path: entry.path.clone(),
            key: entry.key,
        })
    }

    pub(crate) fn observe_commit(&mut self, snapshot: &DocumentSnapshot) -> Result<(), String> {
        let entry = self
            .entries
            .get_mut(&snapshot.id)
            .ok_or_else(|| "document journal was not initialized".to_string())?;
        if snapshot.seq < entry.desired.seq {
            return Err("document journal observed a sequence regression".to_string());
        }
        entry.desired = snapshot.clone();
        Ok(())
    }

    pub(crate) fn request_checkpoint(
        &mut self,
        checkpoint: DurableCheckpoint,
        saved_text: Arc<str>,
    ) -> Result<(), String> {
        if !matches!(
            checkpoint.source,
            DurableSource::AtomicFile | DurableSource::StableFileObservation
        ) {
            return Err(
                "journal pruning requires a verified file durability observation".to_string(),
            );
        }
        let entry = self
            .entries
            .get_mut(&checkpoint.document)
            .ok_or_else(|| "document journal was not initialized".to_string())?;
        if checkpoint.seq > entry.desired.seq {
            return Err("file checkpoint is ahead of the document head".to_string());
        }
        entry.pending_checkpoint = Some(SavedBaseline {
            seq: checkpoint.seq,
            text: saved_text,
        });
        Ok(())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "Plan",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
    pub(crate) fn next_effect(
        &mut self,
        document: DocumentId,
    ) -> Result<Option<JournalEffect>, String> {
        let reseat_fenced = self.reseat_fenced;
        let Some(entry) = self.entries.get_mut(&document) else {
            return Err("document journal was not initialized".to_string());
        };
        if entry_busy(entry) {
            return Ok(None);
        }
        // FIRST, because every other effect is bound to an image this store
        // published: an append or checkpoint planned over a successor's image
        // can only fail its preflight. While the fence stands, nothing is
        // planned at all — the owed re-seat runs first once it falls.
        if entry.reseat_owed && reseat_fenced {
            return Ok(None);
        }
        if entry.reseat_owed {
            let generation = JournalRewriteGeneration(entry.next_rewrite_generation);
            entry.next_rewrite_generation = entry
                .next_rewrite_generation
                .checked_add(1)
                .ok_or_else(|| "journal re-seat generation exhausted".to_string())?;
            let plan = JournalReseatPlan {
                generation,
                document,
                key: entry.key,
                ours: entry.durable_image,
                baseline: entry.baseline,
                baseline_seq: entry.baseline_seq,
                baseline_is_file: entry.baseline_is_file,
                durable_text: entry.durable_text.clone(),
                target_seq: entry.desired.seq,
                target_text: entry.desired.text.clone(),
                retire_legacy: entry.legacy.clone(),
                owner: self.host.owner_id(),
            };
            entry.reseat_owed = false;
            entry.reseat_inflight = Some(plan.clone());
            return Ok(Some(JournalEffect::Reseat {
                path: entry.path.clone(),
                plan,
            }));
        }
        if let Some(saved) = entry.pending_checkpoint.take() {
            let generation = JournalRewriteGeneration(entry.next_rewrite_generation);
            entry.next_rewrite_generation = entry
                .next_rewrite_generation
                .checked_add(1)
                .ok_or_else(|| "journal checkpoint generation exhausted".to_string())?;
            let disk = synthetic_snapshot(document, saved.seq, saved.text.clone());
            let mut plan = checkpoint_plan(
                entry.key,
                generation,
                entry.reducer.durable_seq(),
                entry.durable_image,
                &disk,
                &entry.desired,
            )
            .map_err(|error| format!("could not encode journal checkpoint: {error:?}"))?;
            plan.retire_legacy = entry.legacy.clone();
            plan.owner = self.host.owner_id();
            entry.rewrite_inflight = Some(InflightRewrite {
                plan: plan.clone(),
                checkpoint: saved,
            });
            return Ok(Some(JournalEffect::Rewrite {
                path: entry.path.clone(),
                plan,
            }));
        }
        if entry.desired.seq <= entry.reducer.durable_seq() {
            return Ok(None);
        }
        let mut plan = if entry.desired.seq.0 == entry.reducer.durable_seq().0.saturating_add(1) {
            let edit = single_edit(&entry.durable_text, &entry.desired.text);
            if edit.insert.len() <= MAX_INSERT_BYTES {
                entry
                    .reducer
                    .plan_delta(entry.reducer.durable_seq(), entry.desired.seq, vec![edit])
                    .map_err(|error| format!("could not plan journal delta: {error:?}"))?
            } else {
                entry
                    .reducer
                    .plan_snapshot(&entry.desired)
                    .map_err(|error| format!("could not plan journal snapshot: {error:?}"))?
            }
        } else {
            entry
                .reducer
                .plan_snapshot(&entry.desired)
                .map_err(|error| format!("could not plan journal snapshot: {error:?}"))?
        };
        plan.expected_image = Some(entry.durable_image);
        plan.retire_legacy = entry.legacy.clone();
        plan.owner = self.host.owner_id();
        entry.append_text = Some(entry.desired.text.clone());
        Ok(Some(JournalEffect::Append {
            path: entry.path.clone(),
            key: entry.key,
            plan,
        }))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "Refuse",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
    pub(crate) fn complete_append(
        &mut self,
        document: DocumentId,
        generation: crate::native_document_io::JournalGeneration,
        result: JournalAppendResult,
    ) -> JournalCompletion {
        let Some(entry) = self.entries.get_mut(&document) else {
            return JournalCompletion::Stale;
        };
        let published_image = match &result {
            JournalAppendResult::Committed(proof) => Some(proof.published_image),
            _ => None,
        };
        match entry.reducer.complete(generation, result) {
            JournalReduction::Durable(checkpoint) => {
                if let Some(text) = entry.append_text.take() {
                    entry.durable_text = text;
                }
                if let Some(fingerprint) = published_image {
                    entry.durable_image = fingerprint;
                }
                entry.append_failed = false;
                // The append retired it before it published.
                entry.legacy = None;
                JournalCompletion::Durable {
                    document,
                    seq: checkpoint.seq,
                }
            }
            JournalReduction::Failed { stage, message } => {
                entry.append_text = None;
                entry.append_failed = true;
                JournalCompletion::Failed {
                    document,
                    message: format!("journal append failed at {stage:?}: {message}"),
                }
            }
            JournalReduction::Cancelled => {
                entry.append_text = None;
                entry.append_failed = true;
                JournalCompletion::Failed {
                    document,
                    message: "journal append was cancelled".to_string(),
                }
            }
            JournalReduction::Stale => JournalCompletion::Stale,
        }
    }

    /// Roll back an append that never crossed the worker queue boundary.
    ///
    /// Queue saturation is not an I/O failure: the latest desired snapshot is
    /// still owned by this store and will be planned again after a worker slot
    /// becomes available. Only the small retry registry in `app_documents`
    /// survives this call; the encoded append image is released here.
    pub(crate) fn defer_append(
        &mut self,
        document: DocumentId,
        generation: crate::native_document_io::JournalGeneration,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(&document) else {
            return false;
        };
        if !matches!(
            entry
                .reducer
                .complete(generation, JournalAppendResult::Cancelled),
            JournalReduction::Cancelled
        ) {
            return false;
        }
        entry.append_text = None;
        true
    }

    /// Restore a checkpoint rewrite that was planned but never admitted to the
    /// worker. A newer checkpoint requested meanwhile wins by sequence; either
    /// way, no proven file baseline is consumed merely because the queue was
    /// full.
    pub(crate) fn defer_rewrite(
        &mut self,
        document: DocumentId,
        generation: JournalRewriteGeneration,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(&document) else {
            return false;
        };
        let Some(inflight) = entry.rewrite_inflight.as_ref() else {
            return false;
        };
        if inflight.plan.generation != generation {
            return false;
        }
        let deferred = entry
            .rewrite_inflight
            .take()
            .expect("generation checked against live rewrite")
            .checkpoint;
        if entry
            .pending_checkpoint
            .as_ref()
            .is_none_or(|pending| deferred.seq > pending.seq)
        {
            entry.pending_checkpoint = Some(deferred);
        }
        true
    }

    pub(crate) fn complete_rewrite(
        &mut self,
        document: DocumentId,
        generation: JournalRewriteGeneration,
        result: JournalRewriteResult,
    ) -> JournalCompletion {
        let Some(entry) = self.entries.get_mut(&document) else {
            return JournalCompletion::Stale;
        };
        let Some(plan) = entry.rewrite_inflight.as_ref() else {
            return JournalCompletion::Stale;
        };
        if plan.plan.generation != generation {
            return JournalCompletion::Stale;
        }
        let InflightRewrite { plan, checkpoint } = entry
            .rewrite_inflight
            .take()
            .expect("generation checked against live rewrite");
        match result {
            JournalRewriteResult::Committed(proof) if plan.verifies(proof) => {
                entry.reducer =
                    DraftJournalReducer::new_with_key(document, entry.key, plan.target_seq);
                entry.durable_text = plan.target_text;
                entry.durable_image = plan.fingerprint;
                entry.append_text = None;
                entry.append_failed = false;
                entry.legacy = None;
                // The verified save is the image's first snapshot now.
                entry.baseline = ContentFingerprint::of(checkpoint.text.as_bytes());
                entry.baseline_seq = checkpoint.seq;
                entry.baseline_is_file = true;
                JournalCompletion::Durable {
                    document,
                    seq: plan.target_seq,
                }
            }
            JournalRewriteResult::Committed(_) => JournalCompletion::Failed {
                document,
                message: "journal checkpoint proof did not match the pending image".to_string(),
            },
            JournalRewriteResult::Failed(message) => JournalCompletion::Failed {
                document,
                message: format!("journal checkpoint failed: {message}"),
            },
        }
    }

    /// Reduce a re-seat. `Unchanged` is no news (`Stale`: nothing new is
    /// durable, and the caller drives on); `Republished` makes the fresh image
    /// this store's own, under its own sequence numbers; a failure owes the
    /// re-seat again, so the next drive retries it.
    pub(crate) fn complete_reseat(
        &mut self,
        document: DocumentId,
        generation: JournalRewriteGeneration,
        result: JournalReseatResult,
    ) -> JournalCompletion {
        let Some(entry) = self.entries.get_mut(&document) else {
            return JournalCompletion::Stale;
        };
        if entry
            .reseat_inflight
            .as_ref()
            .is_none_or(|plan| plan.generation != generation)
        {
            return JournalCompletion::Stale;
        }
        let plan = entry
            .reseat_inflight
            .take()
            .expect("generation checked against live re-seat");
        entry.reseat_failed = matches!(result, JournalReseatResult::Failed(_));
        if !entry.reseat_failed {
            // Retired before the re-seat looked at its own image.
            entry.legacy = None;
        }
        match result {
            JournalReseatResult::Unchanged => JournalCompletion::Stale,
            JournalReseatResult::Republished {
                image,
                baseline,
                baseline_seq,
                baseline_is_file,
                preserved: _,
            } => {
                entry.reducer =
                    DraftJournalReducer::new_with_key(document, entry.key, plan.target_seq);
                entry.durable_text = plan.target_text;
                entry.durable_image = image;
                entry.baseline = baseline;
                entry.baseline_seq = baseline_seq;
                entry.baseline_is_file = baseline_is_file;
                entry.append_failed = false;
                JournalCompletion::Durable {
                    document,
                    seq: plan.target_seq,
                }
            }
            JournalReseatResult::Failed(message) => {
                entry.reseat_owed = true;
                JournalCompletion::Failed {
                    document,
                    message: format!("journal re-seat failed: {message}"),
                }
            }
        }
    }

    /// Put back a re-seat that never crossed the worker queue boundary.
    pub(crate) fn defer_reseat(
        &mut self,
        document: DocumentId,
        generation: JournalRewriteGeneration,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(&document) else {
            return false;
        };
        if entry
            .reseat_inflight
            .as_ref()
            .is_none_or(|plan| plan.generation != generation)
        {
            return false;
        }
        entry.reseat_inflight = None;
        entry.reseat_owed = true;
        true
    }

    /// While an update's successor may be restoring documents from these
    /// journals, no re-seat is planned: it would publish over the image the
    /// successor restored from, and the outgoing process's Commit check reads
    /// that image as the successor's copy. The host raises the fence for the
    /// life of a handoff attempt and lowers it once the attempt is over.
    pub(crate) fn set_reseat_fenced(&mut self, fenced: bool) {
        self.reseat_fenced = fenced;
    }

    /// Whether `document`'s journal has nothing left to do: no write on the
    /// worker, no re-seat owed, no checkpoint queued, no legacy journal left to
    /// retire, and its head durable. Only a settled journal is let go with its
    /// document ([`Self::forget`]); a document without one has none to wait on.
    pub(crate) fn settled(&self, document: DocumentId) -> bool {
        self.entries.get(&document).is_none_or(|entry| {
            !entry_busy(entry)
                && !entry.reseat_owed
                && entry.pending_checkpoint.is_none()
                && entry.legacy.is_none()
                && entry.desired.seq == entry.reducer.durable_seq()
        })
    }

    /// Drop the entries of released documents, whose journals are
    /// [`Self::settled`], and let go of their images: this process leaves each
    /// one's owner set, and one that then names no live owner and holds no
    /// draft goes (`owner::settle_releases`, off the event loop). Opening a
    /// document again initializes its journal as any open does.
    pub(crate) fn forget(&mut self, documents: &[DocumentId]) {
        let entries = documents
            .iter()
            .filter_map(|document| {
                debug_assert!(self.settled(*document));
                self.entries.remove(document)
            })
            .collect::<Vec<_>>();
        let Some(owner) = self.host.owner_id() else {
            return;
        };
        let released = entries
            .into_iter()
            .filter_map(|entry| owner::released(entry.path, entry.key))
            .collect::<Vec<_>>();
        if !released.is_empty() {
            owner::settle_releases(owner, released);
        }
    }

    #[cfg(test)]
    pub(crate) fn durable_seq(&self, document: DocumentId) -> Option<Seq> {
        self.entries
            .get(&document)
            .map(|entry| entry.reducer.durable_seq())
    }

    #[cfg(all(test, unix))]
    pub(crate) fn journal_path(&self, document: DocumentId) -> Option<PathBuf> {
        self.entries.get(&document).map(|entry| entry.path.clone())
    }

    /// `canonical_uri`'s journal identity under this store's secret.
    #[cfg(test)]
    pub(crate) fn key_for(&self, canonical_uri: &str) -> JournalDocumentKey {
        self.host.key_for(canonical_uri)
    }

    /// The file `canonical_uri`'s journal lives in.
    #[cfg(test)]
    pub(crate) fn path_for_uri(&self, canonical_uri: &str) -> PathBuf {
        self.host.path_for(self.host.key_for(canonical_uri))
    }

    /// The image this store last published for `document`.
    #[cfg(all(test, unix))]
    pub(crate) fn durable_image(&self, document: DocumentId) -> Option<ContentFingerprint> {
        self.entries.get(&document).map(|entry| entry.durable_image)
    }
}

/// Take the journal's advisory lock for as long as the returned handle lives —
/// how a test makes a peer's (a successor's) journal publication refuse busy.
/// Released by `LOCK_UN` when the handle drops, as the product's own holder is
/// ([`with_journal_lock`]): a lock released by the close alone lives on in the
/// copy of its description any shell another test thread forked meanwhile holds
/// until it execs, and the next take — 25 ms of it on the event loop — is
/// refused for a lock nobody holds.
#[cfg(test)]
pub(crate) fn hold_journal_lock_for_test(
    path: &Path,
) -> crate::native_document_host::HeldAdvisoryLock {
    let lock = open_journal_lock(&journal_lock_path(path).expect("journal lock path"))
        .expect("open journal lock");
    crate::native_document_host::HeldAdvisoryLock::lock(lock).expect("take journal lock")
}

#[cfg(all(test, unix))]
thread_local! {
    /// Some while a [`ForkedJournalLockCopies`] lives on this thread: the
    /// copies of every journal lock description this thread has locked since.
    static FORKED_LOCK_COPIES: std::cell::RefCell<Option<Vec<File>>> =
        const { std::cell::RefCell::new(None) };
}

/// While this lives, every journal lock THIS THREAD takes leaves a second
/// descriptor onto its open file description behind, taken while the lock is
/// held and kept until this drops — exactly what a shell another thread forks
/// at that instant holds until it execs (`aterm_pty`'s hand-rolled `fork`
/// closes only the pty pair and the status pipe before `execve`). The losing
/// interleaving of the fork-copy race, made deterministic: a lock released by
/// the close alone stays taken in the copy, and the next take is refused.
#[cfg(all(test, unix))]
pub(crate) struct ForkedJournalLockCopies(());

#[cfg(all(test, unix))]
impl ForkedJournalLockCopies {
    pub(crate) fn start() -> Self {
        FORKED_LOCK_COPIES.with(|copies| *copies.borrow_mut() = Some(Vec::new()));
        Self(())
    }

    /// How many held locks have been copied so far.
    pub(crate) fn copied(&self) -> usize {
        FORKED_LOCK_COPIES.with(|copies| copies.borrow().as_ref().map_or(0, Vec::len))
    }
}

#[cfg(all(test, unix))]
impl Drop for ForkedJournalLockCopies {
    fn drop(&mut self) {
        FORKED_LOCK_COPIES.with(|copies| drop(copies.borrow_mut().take()));
    }
}

#[cfg(test)]
thread_local! {
    /// What [`between_lock_open_and_take_for_test`] runs.
    static BETWEEN_LOCK_OPEN_AND_TAKE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `then` once, on this thread, between the next journal lock's open and
/// its take: the instant a taker that opened the lock file just before another
/// took it waits through.
#[cfg(test)]
pub(crate) fn between_lock_open_and_take_for_test(then: impl FnOnce() + 'static) {
    BETWEEN_LOCK_OPEN_AND_TAKE.with(|hook| *hook.borrow_mut() = Some(Box::new(then)));
}

#[cfg(test)]
fn lock_opened_for_test() {
    let hook = BETWEEN_LOCK_OPEN_AND_TAKE.with(|hook| hook.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

/// The fork a [`ForkedJournalLockCopies`] stands in for, at the instant the
/// lock is held.
#[cfg(all(test, unix))]
fn copy_held_lock_for_test(lock: &File) {
    FORKED_LOCK_COPIES.with(|copies| {
        if let Some(copies) = copies.borrow_mut().as_mut() {
            copies.push(lock.try_clone().expect("copy the held journal lock"));
        }
    });
}

fn entry_busy(entry: &JournalEntry) -> bool {
    entry.append_text.is_some()
        || entry.rewrite_inflight.is_some()
        || entry.reseat_inflight.is_some()
}

/// The disk half of carrying a draft through an update: read the journal image
/// as the successor will (bounded, no-follow, regular file only), replay it
/// (every record's checksum is checked on the way), and require that it
/// recovers to EXACTLY `head` — byte for byte, which no digest improves on —
/// against the file contents `disk` names, the same test the successor's open
/// makes before it replays a draft ([`DraftJournalHost::inspect_open`]).
///
/// At an update's Commit this reads the successor's own work: a successor
/// republishes the journal of every document it restores with the text it
/// restored, so an image that still recovers to `head` is the proof the
/// successor holds this buffer (or, when its restore failed, that the journal
/// its Recovery tab reopens does).
pub(crate) fn verify_carried_draft(
    candidate: &DraftCarryCandidate,
    head: &str,
    disk: ContentFingerprint,
) -> Result<(), String> {
    let bytes = read_existing_journal(&candidate.path, "the draft journal")?;
    let recovered = recover_journal_for(candidate.key, &bytes)
        .map_err(|error| format!("the draft journal does not replay ({error:?})"))?;
    if recovered.text.as_bytes() != head.as_bytes() {
        return Err("the draft journal does not hold the editor's current text".to_string());
    }
    if recovered.base_content != disk {
        return Err(
            "the draft journal was made against different file contents than are on disk"
                .to_string(),
        );
    }
    Ok(())
}

/// Whether a journal image holds no draft: it is its first snapshot alone, the
/// file as it was opened or last saved, with nothing journaled past it. No
/// writer leaves a draft as one record: `initialize` and a checkpoint write the
/// file's snapshot, then the draft's when there is one; an append adds records,
/// and compaction collapses an image to one only when its latest text is that
/// snapshot at that sequence; a re-seat writes its target alone only when it is
/// the file's contents ([`JournalEntry::baseline_is_file`]), and otherwise after
/// the baseline it copied, or twice when it stands as its own. Such an image is
/// never kept aside: it holds nothing the file did not once hold.
fn holds_no_draft(recovered: &StoredJournalRecovery) -> bool {
    recovered.record_count == 1
}

/// How an open reads one decoded image against the file on disk: the one rule
/// for a keyed journal and a legacy one alike ([`DraftJournalHost::inspect_open`]).
struct OpenReading {
    /// Its draft was made against exactly the file on disk and is not the
    /// file: replayed.
    recover: bool,
    /// Its draft was made against other contents and is not the file: shown is
    /// the file, and the image is kept aside.
    keep_aside: bool,
    /// It holds a draft made against exactly the file on disk, equal to it or
    /// not — the draft an outgoing older aterm's Commit check reads back
    /// ([`LegacyJournal`]).
    made_against_disk: bool,
}

fn read_for_open(recovered: &StoredJournalRecovery, disk_bytes: &[u8]) -> OpenReading {
    // Opened or saved and never edited since: whatever the disk holds now,
    // there is nothing to recover and nothing to keep aside, and `initialize`
    // replaces this image with the file as it is.
    if holds_no_draft(recovered) {
        return OpenReading {
            recover: false,
            keep_aside: false,
            made_against_disk: false,
        };
    }
    let disk = ContentFingerprint::of(disk_bytes);
    let latest = ContentFingerprint::of(recovered.text.as_bytes());
    let made_against_disk = recovered.base_content == disk;
    OpenReading {
        recover: made_against_disk && latest != disk,
        keep_aside: !made_against_disk && latest != disk,
        made_against_disk,
    }
}

/// What a legacy draft that wins an open ([`LegacyJournal`]) does to the keyed
/// image beside it (`keyed`, as it decoded), as the notice that says so: one that does not
/// decode is kept aside as `Corrupt`; one holding a draft the legacy image does
/// not is kept aside as `DiskConflict`; `None` leaves it to be replaced, for it
/// holds nothing the legacy one lacks (no draft, or that draft's migration).
fn keyed_set_aside_by_legacy(
    keyed: &Result<StoredJournalRecovery, crate::native_document_io::JournalError>,
    legacy: &StoredJournalRecovery,
) -> Option<RecoveryNotice> {
    match keyed {
        Err(_) => Some(RecoveryNotice::Corrupt),
        Ok(keyed) if !holds_no_draft(keyed) && keyed.text != legacy.text => {
            Some(RecoveryNotice::DiskConflict)
        }
        Ok(_) => None,
    }
}

impl OpenReading {
    fn apply(&self, decision: &mut JournalOpenDecision, recovered: StoredJournalRecovery) {
        if self.recover {
            aterm_log::info!(
                "recovered an unsaved draft from {} journal records",
                recovered.record_count
            );
            decision.recovered_text = Some(recovered.text);
            decision.notice = Some(RecoveryNotice::Recovered);
        } else if self.keep_aside {
            decision.notice = Some(RecoveryNotice::DiskConflict);
        }
    }
}

/// The first snapshot of a journal image for `key` — the file contents its
/// draft was made against. `None` for an image that does not decode, belongs
/// to another document, or does not open with a snapshot.
fn journal_baseline_text(key: JournalDocumentKey, bytes: &[u8]) -> Option<String> {
    let records = crate::native_document_io::decode_journal(bytes).ok()?;
    let first = records.into_iter().next()?;
    if !first.belongs_to_key(key) {
        return None;
    }
    match first.payload {
        crate::native_document_io::JournalPayload::Snapshot(text) => Some(text),
        crate::native_document_io::JournalPayload::Delta(_) => None,
    }
}

/// `patience` carries the caller's thread, exactly as in [`execute_journal_append`].
///
/// Under the journal lock: an image that is still this store's is left alone.
/// Anything else is preserved aside unless it holds nothing this store did not
/// have (its text is the store's durable text or its target, or it holds no
/// draft at all: [`holds_no_draft`]), then replaced by an image recovering to
/// the target:
/// - `[baseline, target]` when the image found there still opens with this
///   store's baseline, copied from it;
/// - `[target]` when the target is this store's baseline and that baseline is
///   the file's contents ([`JournalReseatPlan::baseline_is_file`]): a clean
///   image;
/// - `[target, target]` otherwise: the target stands as its own baseline, so a
///   later open over any other file contents reports a conflict and keeps it,
///   and the second record keeps it from reading as clean.
///
/// Nothing is deleted on any path.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateEditorCarry",
        action = "Reseat",
        project = "aterm_gui::editor_carry_conformance::Rig::project"
    )
)]
pub(crate) fn execute_journal_reseat(
    path: &Path,
    plan: &JournalReseatPlan,
    patience: JournalLockPatience,
) -> JournalReseatResult {
    if let Err(message) = retire_owed_legacy(plan.retire_legacy.as_ref(), patience) {
        return JournalReseatResult::Failed(message);
    }
    let result = with_journal_lock(path, patience, || {
        let existing = read_journal_image(path).map_err(|error| format!("preflight: {error}"))?;
        let found = existing
            .as_deref()
            .map_or_else(JournalImageGeneration::missing, JournalImageGeneration::of);
        if found.exists && found.fingerprint == plan.ours {
            owner::adopt(path, plan.ours, plan.owner);
            return Ok(None);
        }
        let found_baseline = existing
            .as_deref()
            .and_then(|bytes| journal_baseline_text(plan.key, bytes));
        let recovered = existing
            .as_deref()
            .and_then(|bytes| recover_journal_for(plan.key, bytes).ok());
        let holds_nothing_new = recovered.as_ref().is_some_and(|recovered| {
            recovered.text == *plan.durable_text
                || recovered.text == *plan.target_text
                || holds_no_draft(recovered)
        });
        let preserved = found.exists && !holds_nothing_new;
        if preserved {
            preserve_journal_locked(path, found)?;
        }
        let baseline =
            found_baseline.filter(|text| ContentFingerprint::of(text.as_bytes()) == plan.baseline);
        let target = synthetic_snapshot(plan.document, plan.target_seq, plan.target_text.clone());
        let (records, baseline_fingerprint, baseline_seq, baseline_is_file) = match baseline {
            Some(text) if text.as_str() != &*plan.target_text => {
                let base = synthetic_snapshot(plan.document, plan.baseline_seq, Arc::from(text));
                (
                    vec![
                        JournalRecord::snapshot_for(plan.key, &base),
                        JournalRecord::snapshot_for(plan.key, &target),
                    ],
                    plan.baseline,
                    plan.baseline_seq,
                    plan.baseline_is_file,
                )
            }
            // The target is the file's contents, copied or not: a clean image.
            _ if plan.baseline_is_file
                && ContentFingerprint::of(plan.target_text.as_bytes()) == plan.baseline =>
            {
                (
                    vec![JournalRecord::snapshot_for(plan.key, &target)],
                    plan.baseline,
                    plan.target_seq,
                    true,
                )
            }
            // A draft whose baseline is not there to copy, or is itself a
            // draft, stands as its own, so a later open over any other file
            // contents reports a conflict and keeps it. Recorded twice: alone it
            // would read as a clean image ([`holds_no_draft`]), which that open
            // replaces.
            _ => {
                let draft = JournalRecord::snapshot_for(plan.key, &target);
                (
                    vec![draft.clone(), draft],
                    ContentFingerprint::of(plan.target_text.as_bytes()),
                    plan.target_seq,
                    false,
                )
            }
        };
        let bytes = encode_journal(&records).map_err(|error| format!("encode: {error:?}"))?;
        atomic_replace_locked(path, &bytes, found, plan.owner)
            .map_err(|error| error.to_string())?;
        Ok(Some(JournalReseatResult::Republished {
            image: ContentFingerprint::of(&bytes),
            baseline: baseline_fingerprint,
            baseline_seq,
            baseline_is_file,
            preserved,
        }))
    });
    match result {
        Ok(None) => JournalReseatResult::Unchanged,
        Ok(Some(republished)) => republished,
        Err(message) => JournalReseatResult::Failed(message),
    }
}

fn synthetic_snapshot(document: DocumentId, seq: Seq, text: Arc<str>) -> DocumentSnapshot {
    DocumentSnapshot {
        id: document,
        seq,
        text,
    }
}

/// Narrow two document snapshots to the one replaced range between their common prefix and
/// common suffix.
///
/// PERF: this runs on the UI thread for EVERY committed edit (`next_effect` ← the editor's
/// per-keystroke commit), and both scans cover the whole UNCHANGED region — for the common
/// append-at-end that is the entire document, twice. Comparing `char`s meant two decoding
/// iterators zipped together: several times the cost of a byte compare, and unvectorisable.
/// So compare bytes and then snap the divergence point back onto a `char` boundary.
///
/// That is exactly equivalent, by UTF-8 self-synchronisation. Two strings agreeing on bytes
/// `[0, k)` have the SAME char boundaries inside that window, so the largest boundary `b <= k`
/// bounds a run of identical chars; and the char straddling `b` differs in the two strings
/// (they differ at byte `k`, which lies inside it in both). The mirror argument holds for the
/// suffix: a byte that is a boundary from one end is not a continuation byte, so it is a
/// boundary in both tails. The returned [`JournalEdit`] is therefore bit-identical — which it
/// must be, since `insert.len()` picks delta-vs-snapshot encoding against `MAX_INSERT_BYTES`.
fn single_edit(before: &str, after: &str) -> JournalEdit {
    let mut prefix = before
        .as_bytes()
        .iter()
        .zip(after.as_bytes())
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| before.len().min(after.len()));
    while prefix > 0 && !before.is_char_boundary(prefix) {
        prefix -= 1;
    }
    // The tails already exclude the prefix, so the suffix can never overlap it.
    let before_tail = &before[prefix..];
    let after_tail = &after[prefix..];
    let mut suffix = before_tail
        .as_bytes()
        .iter()
        .rev()
        .zip(after_tail.as_bytes().iter().rev())
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| before_tail.len().min(after_tail.len()));
    // Snap DOWN (shrinking the matched tail) so the split lands on a boundary of both tails.
    while suffix > 0 && !before_tail.is_char_boundary(before_tail.len() - suffix) {
        suffix -= 1;
    }
    JournalEdit {
        range: prefix..before.len().saturating_sub(suffix),
        insert: after[prefix..after.len().saturating_sub(suffix)].to_string(),
    }
}

impl DraftJournalHost {
    pub(crate) fn system_default() -> Result<Self, String> {
        #[cfg(test)]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(1);
            // A RECYCLED PID MUST NEVER RESOLVE TO A DEAD RUN'S ROOT. These
            // roots are draft-recovery journals, so a pid+ordinal name alone let
            // macOS pid recycling hand a fresh test process a prior run's
            // unsaved draft — which crash recovery then dutifully replayed into
            // the new test's document (the proven six-tests-at-once
            // app_documents flake: one Enter on "abc" landed on a resurrected
            // "abc\n" and read back "abc\n\n"). The process root is stamped per
            // process INSTANCE, and a same-named corpse is removed.
            //
            // AND THEN TAKE THEM BACK. Every call used to leave one directory in
            // the system temp dir forever; on m17-tower that reached 224,002 of
            // them — enough that `/tmp`, a RAM-backed tmpfs there, held them in
            // memory and the box ran itself out of swap. So every root for this
            // process nests under ONE process root, which goes when the process
            // exits and is swept by the next run when it was killed first
            // ([`crate::test_scratch::process_root`]).
            let root = crate::test_scratch::process_root(TEST_DRAFT_ROOT_PREFIX)
                .join(NEXT.fetch_add(1, Ordering::Relaxed).to_string());
            let _ = std::fs::remove_dir_all(&root);
            Self::new(root)
        }
        #[cfg(not(test))]
        {
            let root = default_state_root().ok_or_else(|| {
                "could not locate a private state directory for document recovery".to_string()
            })?;
            let host = Self::new(root.join("drafts"))?;
            // After this host's own lease is taken, so it can never read this
            // process as gone.
            #[cfg(any(unix, windows))]
            owner::spawn_launch_sweep(host.root.clone());
            Ok(host)
        }
    }

    /// The host over `root`, with the journal secret read from it (or minted
    /// into it): a secret this process cannot use refuses the whole store, as
    /// a root it cannot make private does ([`load_or_mint_journal_secret`]).
    /// Its owner lease is taken here; without one the store still works, and
    /// what it publishes is never swept.
    pub(crate) fn new(root: PathBuf) -> Result<Self, String> {
        create_private_dir(&root).map_err(|error| error.to_string())?;
        let secret = load_or_mint_journal_secret(&root)?;
        let owner = owner::establish(&root)
            .inspect_err(|error| {
                if cfg!(any(unix, windows)) {
                    aterm_log::warn!("draft journals published unswept: {error}");
                }
            })
            .ok()
            .map(Arc::new);
        Ok(Self {
            root,
            secret,
            owner,
        })
    }

    /// The owner identity this host names on what it publishes.
    pub(crate) fn owner_id(&self) -> Option<JournalOwnerId> {
        self.owner.as_ref().map(|owner| owner.id())
    }

    /// `canonical_uri`'s journal identity under this install's secret.
    pub(crate) fn key_for(&self, canonical_uri: &str) -> JournalDocumentKey {
        JournalDocumentKey::keyed(&self.secret, canonical_uri)
    }

    pub(crate) fn inspect_open(
        &self,
        canonical_uri: &str,
        disk_bytes: &[u8],
    ) -> Result<JournalOpenDecision, String> {
        let key = self.key_for(canonical_uri);
        let path = self.path_for(key);
        let mut decision = JournalOpenDecision {
            key,
            path: path.clone(),
            recovered_text: None,
            notice: None,
            preserve_existing: false,
            expected_image: JournalImageGeneration::missing(),
            expected_clean: false,
            legacy: None,
        };
        let keyed = read_journal_image(&path)
            .map_err(|error| format!("could not read recovery journal: {error}"))?;
        let keyed_recovery = keyed.as_deref().map(|bytes| {
            decision.expected_image = JournalImageGeneration::of(bytes);
            recover_journal_for(key, bytes)
        });
        decision.expected_clean = keyed_recovery
            .as_ref()
            .is_some_and(|recovered| recovered.as_ref().is_ok_and(holds_no_draft));
        let legacy_key = JournalDocumentKey::legacy_unkeyed(canonical_uri);
        let legacy_path = self.legacy_path_for(legacy_key);
        let legacy = read_journal_image(&legacy_path)
            .map_err(|error| format!("could not read legacy recovery journal: {error}"))?;
        if let Some(legacy_bytes) = legacy {
            let image = JournalImageGeneration::of(&legacy_bytes);
            match recover_journal_for(legacy_key, &legacy_bytes) {
                // A LEGACY IMAGE THAT CARRIES NOTHING NEVER DECIDES
                // ([`LegacyJournal`]): no draft, or one made against other
                // contents that the file now equals. It is retired at the open,
                // and the keyed journal (if any) decides below.
                Ok(recovered) => {
                    let reading = read_for_open(&recovered, disk_bytes);
                    if reading.made_against_disk || reading.keep_aside {
                        // A LEGACY DRAFT WINS while it exists.
                        decision.legacy = Some(LegacyJournal {
                            path: legacy_path,
                            image,
                            carried: reading.made_against_disk,
                            keep_aside: reading.keep_aside,
                        });
                        let set_aside = keyed_recovery
                            .as_ref()
                            .and_then(|keyed| keyed_set_aside_by_legacy(keyed, &recovered));
                        reading.apply(&mut decision, recovered);
                        if let Some(notice) = set_aside {
                            aterm_log::warn!(
                                "a legacy recovery journal was written after this document's \
                                 keyed one; the keyed draft is kept aside"
                            );
                            decision.preserve_existing = true;
                            // Never silently: a legacy draft that is the file
                            // itself says nothing, so the set-aside speaks.
                            decision.notice.get_or_insert(notice);
                        }
                        return Ok(decision);
                    }
                    decision.legacy = Some(LegacyJournal {
                        path: legacy_path,
                        image,
                        carried: false,
                        keep_aside: false,
                    });
                }
                // No readable draft of anyone's: kept aside and retired, and the
                // keyed journal (if any) decides.
                Err(error) => {
                    aterm_log::warn!(
                        "legacy recovery journal did not decode; kept aside: {error:?}"
                    );
                    decision.legacy = Some(LegacyJournal {
                        path: legacy_path,
                        image,
                        carried: false,
                        keep_aside: true,
                    });
                    if keyed.is_none() {
                        decision.notice = Some(RecoveryNotice::Corrupt);
                    }
                }
            }
        }
        let Some(keyed_recovery) = keyed_recovery else {
            return Ok(decision);
        };
        match keyed_recovery {
            Ok(recovered) => {
                let reading = read_for_open(&recovered, disk_bytes);
                decision.preserve_existing = reading.keep_aside;
                reading.apply(&mut decision, recovered);
            }
            Err(error) => {
                aterm_log::warn!("recovery journal did not decode; kept aside: {error:?}");
                decision.notice = Some(RecoveryNotice::Corrupt);
                decision.preserve_existing = true;
            }
        }
        Ok(decision)
    }

    /// Publish a fresh, sequence-aligned image before any document view becomes
    /// visible. A conflicting/corrupt prior image is renamed, never deleted.
    ///
    /// ALWAYS ON THE EVENT LOOP, hence no patience parameter: the one production
    /// caller is `App::install_admitted_document` (`app_documents.rs`),
    /// which runs it SYNCHRONOUSLY inside a `&mut self` handler because the
    /// `InitializedJournal` it returns is what the open flow needs to build the
    /// view. There is nowhere to hand it: the document worker's queue carries
    /// appends and rewrites, whose results arrive later through `Wake`, and an
    /// open cannot proceed on a promise. So this path takes the frame-sized
    /// budget and, past it, REPORTS BUSY rather than blocking longer — the
    /// refusal `manual_open_reports_busy_held_journal_lock_and_retries_cleanly`
    /// pins, carrying the "retry opening Manual"
    /// guidance that makes the user's retry the recovery path.
    pub(crate) fn initialize(
        &self,
        decision: JournalOpenDecision,
        disk: &DocumentSnapshot,
        current: &DocumentSnapshot,
    ) -> Result<InitializedJournal, String> {
        if disk.id != current.id {
            return Err("journal initialization mixed document identities".to_string());
        }
        let mut records = vec![JournalRecord::snapshot_for(decision.key, disk)];
        if current.seq != disk.seq || current.text != disk.text {
            records.push(JournalRecord::snapshot_for(decision.key, current));
        }
        let bytes = encode_journal(&records).map_err(|error| format!("{error:?}"))?;
        let image_fingerprint = ContentFingerprint::of(&bytes);
        with_journal_lock(&decision.path, JournalLockPatience::EventLoop, || {
            let current = observe_journal_image(&decision.path)?;
            // A clean image gone since the inspection (the launch sweep took
            // it) decided nothing: publish over its absence.
            let swept = decision.expected_clean && !current.exists;
            if current != decision.expected_image && !swept {
                return Err(format!(
                    "journal changed after recovery inspection (expected {:?}, found {:?}); reopen \
                     the document before initializing recovery",
                    decision.expected_image, current
                ));
            }
            if decision.preserve_existing && current.exists {
                self.preserve_locked(&decision.path, current)?;
            }
            atomic_replace_locked(&decision.path, &bytes, current, self.owner_id())
                .map_err(|error| error.to_string())
        })?;
        // The keyed image is published: a legacy journal whose draft was not
        // carried has nothing left to give, and goes now. A carried one stays
        // until the first write ([`LegacyJournal`]); so does one this cannot
        // retire yet, whose retirement that write then owes.
        let legacy = decision.legacy.filter(|legacy| {
            if legacy.carried {
                return true;
            }
            match retire_legacy_journal(legacy, JournalLockPatience::EventLoop) {
                Ok(()) => false,
                Err(error) => {
                    aterm_log::warn!(
                        "legacy recovery journal not retired at open, so before the first \
                         write: {error}"
                    );
                    true
                }
            }
        });
        Ok(InitializedJournal {
            key: decision.key,
            path: decision.path,
            durable_seq: current.seq,
            durable_text: current.text.clone(),
            image_fingerprint,
            notice: decision.notice,
            legacy,
        })
    }

    /// A keyed journal's name: the key and a `.mac` infix, a shape no legacy
    /// journal (`<key>.atdj`) has, so neither is ever read as the other.
    pub(crate) fn path_for(&self, key: JournalDocumentKey) -> PathBuf {
        self.root.join(format!("{:016x}.mac.atdj", key.0))
    }

    /// The name an older aterm gave the journal of a URI with this legacy key.
    fn legacy_path_for(&self, legacy: JournalDocumentKey) -> PathBuf {
        self.root.join(format!("{:016x}.atdj", legacy.0))
    }

    /// Set the existing journal aside as `<stem>.preserved-<n>.atdj`, the first
    /// free `n`, beside the live one.
    fn preserve_locked(&self, path: &Path, expected: JournalImageGeneration) -> Result<(), String> {
        debug_assert_eq!(path.parent(), Some(self.root.as_path()));
        preserve_journal_locked(path, expected)
    }
}

/// [`DraftJournalHost::preserve_locked`] for a caller holding only the path —
/// the worker's re-seat ([`execute_journal_reseat`]). The preserved copy is a
/// hard link beside the live journal, verified to be `expected` before the
/// directory is synced; the caller holds the journal lock.
fn preserve_journal_locked(path: &Path, expected: JournalImageGeneration) -> Result<(), String> {
    let root = path
        .parent()
        .ok_or_else(|| "journal has no parent directory".to_string())?;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("draft");
    for suffix in 1..=10_000_u32 {
        let candidate = root.join(format!("{stem}.preserved-{suffix}.atdj"));
        match fs::hard_link(path, &candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("could not preserve recovery journal: {error}"));
            }
        }
        let preserved = observe_journal_image(&candidate);
        if !matches!(preserved, Ok(actual) if actual == expected) {
            let _ = fs::remove_file(&candidate);
            return Err(match preserved {
                Ok(actual) => format!(
                    "journal changed while preserving it (expected {expected:?}, found \
                     {actual:?})"
                ),
                Err(error) => format!("could not validate preserved recovery journal: {error}"),
            });
        }
        // Ordered: the link lands before any later replace; a lost one leaves the
        // original image under its own name.
        return sync_directory(root, Flush::Ordered)
            .map_err(|error| format!("could not sync preserved recovery journal: {error}"));
    }
    Err("too many preserved recovery journals for this document".to_string())
}

/// Retire a [`LegacyJournal`], under its own lock: gone already is retired;
/// still the image the open read, it is set aside first when its draft was not
/// carried, then unlinked; any other image an older aterm wrote after the read,
/// newer than anything migrated from it, so it is LEFT for the next open to
/// read and this refuses — the write that owed the retirement fails closed
/// rather than publish a keyed image the next open would set aside.
fn retire_legacy_journal(
    legacy: &LegacyJournal,
    patience: JournalLockPatience,
) -> Result<(), String> {
    with_journal_lock(&legacy.path, patience, || {
        let found = observe_journal_image(&legacy.path)?;
        if !found.exists {
            return Ok(());
        }
        if found != legacy.image {
            return Err(format!(
                "the legacy recovery journal {} changed after it was read (an older aterm \
                 wrote it); reopen the document to read it",
                legacy.path.display()
            ));
        }
        if legacy.keep_aside {
            preserve_journal_locked(&legacy.path, found)?;
        }
        fs::remove_file(&legacy.path)
            .map_err(|error| format!("could not retire legacy recovery journal: {error}"))?;
        let root = legacy
            .path
            .parent()
            .ok_or_else(|| "journal has no parent directory".to_string())?;
        // Ordered: a lost unlink leaves the legacy journal to be read and retired
        // again at the next open.
        sync_directory(root, Flush::Ordered)
            .map_err(|error| format!("could not sync retired legacy recovery journal: {error}"))
    })
}

/// [`retire_legacy_journal`] for a write plan that owes one, before the write
/// takes its own lock (so no thread ever holds two journal locks).
fn retire_owed_legacy(
    legacy: Option<&LegacyJournal>,
    patience: JournalLockPatience,
) -> Result<(), String> {
    legacy.map_or(Ok(()), |legacy| {
        retire_legacy_journal(legacy, patience).map_err(|error| format!("legacy journal: {error}"))
    })
}

/// The per-install journal secret's file in the draft directory.
const JOURNAL_SECRET_FILE: &str = ".journal-key";
/// Its format: this magic, then the [`JOURNAL_SECRET_LEN`] secret bytes.
const JOURNAL_SECRET_MAGIC: [u8; 8] = *b"ATDJKEY1";
const JOURNAL_SECRET_FILE_LEN: usize = JOURNAL_SECRET_MAGIC.len() + JOURNAL_SECRET_LEN;

/// The journal secret of the draft directory `root`, minted when it has none.
///
/// RULINGS (hashclock-2, 2026-10-10), all fail-closed and reversible:
/// - MISSING: minted — 32 CSPRNG bytes ([`aterm_uds::rand::fill`]; no entropy,
///   no secret, no store) written to a temporary created 0600 with
///   `O_EXCL|O_NOFOLLOW`, synced, then published by `link(2)`, which never
///   replaces: two aterms minting at once both end up reading the one that
///   landed, never a half-written file. Keyed journals left by a secret that
///   was deleted stay on disk untouched; no key reaches them again.
/// - UNREADABLE OR MALFORMED (a link, FIFO or other non-regular file, another
///   owner, group or other permission bits, a wrong length or magic, all zeros,
///   an I/O error): REFUSED, and never replaced. Drafts keyed under it are read
///   only through it, so minting over it would orphan every one; the store is
///   unavailable instead (document opens say why, naming the file) and every
///   journal — keyed or legacy — stays exactly where it is until the file is
///   repaired, restored, or removed by the person who decides those drafts go.
/// - NEVER A FALLBACK TO THE UNKEYED NAME for a new journal.
fn load_or_mint_journal_secret(root: &Path) -> Result<JournalSecret, String> {
    let path = root.join(JOURNAL_SECRET_FILE);
    let unusable = |error: String| {
        format!(
            "document recovery is unavailable: its key {} is unusable ({error}); it is never \
             replaced, because the drafts journaled under it are read through it — repair or \
             restore it, or remove it to give up those drafts",
            path.display()
        )
    };
    if let Some(secret) = read_journal_secret(&path).map_err(unusable)? {
        return Ok(secret);
    }
    mint_journal_secret(root, &path).map_err(|error| {
        format!(
            "document recovery is unavailable: could not create its key {}: {error}",
            path.display()
        )
    })
}

/// Read the journal secret back strictly; `Ok(None)` only when there is none.
fn read_journal_secret(path: &Path) -> Result<Option<JournalSecret>, String> {
    let mut file = match open_journal_secret(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.file_type().is_file() {
        return Err("not a regular file".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != crate::control_auth::our_uid() {
            return Err("owned by another user".to_string());
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(format!(
                "mode {:o}: open to other users, not 600",
                metadata.mode() & 0o777
            ));
        }
    }
    if metadata.len() != JOURNAL_SECRET_FILE_LEN as u64 {
        return Err(format!(
            "{} bytes, not {JOURNAL_SECRET_FILE_LEN}",
            metadata.len()
        ));
    }
    let mut bytes = [0u8; JOURNAL_SECRET_FILE_LEN];
    file.read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    if file
        .read(&mut [0u8; 1])
        .map_err(|error| error.to_string())?
        != 0
    {
        return Err("it grew while it was read".to_string());
    }
    decode_journal_secret(&bytes).map(Some)
}

fn decode_journal_secret(bytes: &[u8; JOURNAL_SECRET_FILE_LEN]) -> Result<JournalSecret, String> {
    let (magic, secret) = bytes.split_at(JOURNAL_SECRET_MAGIC.len());
    if magic != JOURNAL_SECRET_MAGIC {
        return Err("not a draft-journal key".to_string());
    }
    let mut raw = [0u8; JOURNAL_SECRET_LEN];
    raw.copy_from_slice(secret);
    JournalSecret::from_bytes(raw).ok_or_else(|| "all zeros".to_string())
}

/// Mint and publish a secret (see [`load_or_mint_journal_secret`]), or read
/// the one another aterm published first.
fn mint_journal_secret(root: &Path, path: &Path) -> Result<JournalSecret, String> {
    let mut raw = [0u8; JOURNAL_SECRET_LEN];
    aterm_uds::rand::fill(&mut raw).map_err(|error| format!("no OS entropy: {error}"))?;
    let secret = JournalSecret::from_bytes(raw).ok_or_else(|| "no OS entropy".to_string())?;
    let mut image = Vec::with_capacity(JOURNAL_SECRET_FILE_LEN);
    image.extend_from_slice(&JOURNAL_SECRET_MAGIC);
    image.extend_from_slice(secret.as_bytes());
    let mut temporary = None;
    for nonce in 1..=10_000_u32 {
        let candidate = root.join(format!(
            "{JOURNAL_SECRET_FILE}.{}-{nonce}.tmp",
            std::process::id()
        ));
        match create_journal_secret_temporary(&candidate) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    let (temporary_path, mut file) =
        temporary.ok_or_else(|| "no free temporary name".to_string())?;
    // Ordered: every journal keyed by this secret is written after this barrier, so
    // a secret lost to a power loss took every such journal with it and a fresh mint
    // is right; the barrier before the link is the load-bearing part, since an
    // unusable key is never replaced.
    let written = file
        .write_all(&image)
        .and_then(|()| aterm_durable::ordered(&file));
    drop(file);
    let published = written.and_then(|()| fs::hard_link(&temporary_path, path));
    let _ = fs::remove_file(&temporary_path);
    match published {
        Ok(()) => {
            sync_directory(root, Flush::Ordered).map_err(|error| format!("sync: {error}"))?;
            Ok(secret)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            read_journal_secret(path)?.ok_or_else(|| "it vanished as it was read".to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(unix)]
fn create_journal_secret_temporary(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(unix))]
fn create_journal_secret_temporary(path: &Path) -> std::io::Result<File> {
    // `create_new` fails on any existing name, a link included.
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Open the secret without following or waiting on a hostile final component;
/// the handle, not the pathname, is then proved a regular file.
#[cfg(unix)]
fn open_journal_secret(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(windows)]
fn open_journal_secret(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a reparse point",
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_journal_secret(path: &Path) -> std::io::Result<File> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    File::open(path)
}

/// `patience` is the CALLER's declaration of which thread it is on, because that
/// is the only place the answer is known: `native_document_queue` runs this on the
/// `aterm-native-document` worker, `App::drive_document_journal_once` runs it
/// inline on the thread that owns `App`. See [`JournalLockPatience`].
pub(crate) fn execute_journal_append(
    path: &Path,
    key: JournalDocumentKey,
    plan: &JournalAppendPlan,
    patience: JournalLockPatience,
) -> JournalAppendResult {
    if let Err(message) = retire_owed_legacy(plan.retire_legacy.as_ref(), patience) {
        return JournalAppendResult::Failed {
            stage: JournalStage::Append,
            message,
        };
    }
    let result = with_journal_lock(path, patience, || {
        let existing = read_existing_journal(path, "preflight")?;
        let expected_image = plan
            .expected_image
            .ok_or_else(|| "preflight: journal plan has no bound disk image".to_string())?;
        let actual = JournalImageGeneration::of(&existing);
        if actual.fingerprint != expected_image {
            return Err(format!(
                "preflight: journal image changed (expected {:?}, found {:?})",
                expected_image, actual.fingerprint
            ));
        }
        let recovered =
            recover_journal_for(key, &existing).map_err(|error| format!("preflight: {error:?}"))?;
        if recovered.durable_seq != plan.base_durable {
            return Err(format!(
                "preflight: journal sequence changed (expected {}, found {})",
                plan.base_durable.0, recovered.durable_seq.0
            ));
        }
        let combined_len = existing
            .len()
            .checked_add(plan.bytes.len())
            .ok_or_else(|| "append length overflow".to_string())?;
        if combined_len > MAX_JOURNAL_BYTES {
            return Err("journal reached its bounded size limit".to_string());
        }
        let mut combined = existing;
        combined.extend_from_slice(&plan.bytes);
        let image = compact_journal(key, &combined, COMPACT_RECORDS, COMPACT_BYTES)
            .map_err(|error| format!("compact: {error:?}"))?;
        let published_image = ContentFingerprint::of(&image);
        atomic_replace_locked(path, &image, actual, plan.owner)
            .map_err(|error| error.to_string())?;
        Ok(JournalAppendProof {
            appended_len: plan.bytes.len(),
            encoded_fingerprint: plan.encoded_fingerprint,
            published_image,
            file_synced: true,
            renamed_over_journal: true,
            directory_synced: true,
        })
    });
    match result {
        Ok(proof) => JournalAppendResult::Committed(proof),
        Err(message) => JournalAppendResult::Failed {
            stage: JournalStage::Append,
            message,
        },
    }
}

/// `patience` carries the caller's thread, exactly as in [`execute_journal_append`].
pub(crate) fn execute_journal_rewrite(
    path: &Path,
    plan: &JournalRewritePlan,
    patience: JournalLockPatience,
) -> JournalRewriteResult {
    if let Err(message) = retire_owed_legacy(plan.retire_legacy.as_ref(), patience) {
        return JournalRewriteResult::Failed(message);
    }
    let result = with_journal_lock(path, patience, || {
        let existing = read_existing_journal(path, "preflight")?;
        let actual = JournalImageGeneration::of(&existing);
        if actual.fingerprint == plan.fingerprint {
            // A prior attempt may have published the exact image but lost its
            // post-publication proof. Re-publishing the already verified bytes
            // avoids a second pathname open (and its final-component swap window).
            return atomic_replace_locked(path, &plan.bytes, actual, plan.owner)
                .map_err(|error| format!("reconcile: {error}"));
        }
        if actual.fingerprint != plan.expected_image {
            return Err(format!(
                "preflight: journal image changed (expected {:?}, found {:?})",
                plan.expected_image, actual.fingerprint
            ));
        }
        let recovered = recover_journal_for(plan.key, &existing)
            .map_err(|error| format!("preflight: {error:?}"))?;
        if recovered.durable_seq != plan.base_durable {
            return Err(format!(
                "preflight: journal sequence changed (expected {}, found {})",
                plan.base_durable.0, recovered.durable_seq.0
            ));
        }
        atomic_replace_locked(path, &plan.bytes, actual, plan.owner)
            .map_err(|error| error.to_string())
    });
    match result {
        Ok(()) => JournalRewriteResult::Committed(JournalRewriteProof {
            bytes_len: plan.bytes.len(),
            fingerprint: plan.fingerprint,
            file_synced: true,
            renamed_over_journal: true,
            directory_synced: true,
        }),
        Err(error) => JournalRewriteResult::Failed(error.to_string()),
    }
}

pub(crate) fn checkpoint_plan(
    key: JournalDocumentKey,
    generation: JournalRewriteGeneration,
    base_durable: Seq,
    expected_image: ContentFingerprint,
    disk: &DocumentSnapshot,
    current: &DocumentSnapshot,
) -> Result<JournalRewritePlan, JournalError> {
    let mut records = vec![JournalRecord::snapshot_for(key, disk)];
    if current.seq != disk.seq || current.text != disk.text {
        records.push(JournalRecord::snapshot_for(key, current));
    }
    let bytes = encode_journal(&records)?;
    let fingerprint = ContentFingerprint::of(&bytes);
    Ok(JournalRewritePlan {
        generation,
        key,
        base_durable,
        expected_image,
        target_seq: current.seq,
        target_text: current.text.clone(),
        bytes: Arc::from(bytes),
        fingerprint,
        retire_legacy: None,
        owner: None,
    })
}

/// The one name every test draft root in this process hangs under.
#[cfg(test)]
const TEST_DRAFT_ROOT_PREFIX: &str = "aterm-test-drafts-";

/// aterm's shared per-user state root — the ONE rule
/// ([`aterm_types::dirs::state_dir`]), so the journal, the harness and the
/// session identities never disagree about the directory.
#[cfg(not(test))]
fn default_state_root() -> Option<PathBuf> {
    aterm_types::dirs::state_dir()
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    // Missing ancestors `0755` whatever the umask: the state root is the atpkg prefix's
    // parent on macOS, and on Linux it can be the first to make `~/.local` above it.
    aterm_types::fs_restricted::create_dir_all_unshared(path, 0o700)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    if !directory.metadata()?.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "recovery journal root is not a real directory",
        ));
    }
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(windows)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "recovery journal root is not a real directory",
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "recovery journal root is not a real directory",
        ));
    }
    Ok(())
}

fn journal_lock_path(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "journal has no parent directory".to_string())?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("draft.atdj");
    Ok(parent.join(format!(".{name}.lock")))
}

fn journal_not_regular(kind: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("recovery journal {kind} is not a regular non-link file"),
    )
}

/// Open the sibling advisory lock without following or waiting on a hostile
/// final component. The opened handle, rather than pathname metadata, is the
/// object proved regular and subsequently locked.
#[cfg(unix)]
fn open_journal_lock(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(journal_not_regular("lock"));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_journal_lock(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(journal_not_regular("lock"));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_journal_lock(path: &Path) -> std::io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(journal_not_regular("lock"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(journal_not_regular("lock"));
    }
    Ok(file)
}

fn with_journal_lock<T>(
    path: &Path,
    patience: JournalLockPatience,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "journal has no parent directory".to_string())?;
    create_private_dir(parent).map_err(|error| format!("journal lock directory: {error}"))?;
    let lock_path = journal_lock_path(path)?;
    // Released by `LOCK_UN` on every exit, not by the close: a shell forked while
    // this runs would otherwise keep the lock until its exec, and the event loop's
    // next take has 25 ms (the product fd-hygiene sweep of 2026-09-27).
    let _held = take_linked_journal_lock(&lock_path, path, patience)?;
    #[cfg(all(test, unix))]
    copy_held_lock_for_test(_held.file());
    operation()
}

/// Take the journal's lock on the file its lock path names. A release or the
/// launch sweep (`owner`) unlinks a journal's lock file while it holds it, so a
/// taker that opened the name before that unlink and waited would, once it has
/// the lock, hold one nobody else will ever take — a later taker creates the
/// name afresh — and two writers would be in at once. So the lock is proved,
/// once held, to be the file its name still names, and the name is opened
/// again when it is not. An unlinker holds the lock it removes, so a lock held
/// and proved named stays named until it is let go.
fn take_linked_journal_lock(
    lock_path: &Path,
    path: &Path,
    patience: JournalLockPatience,
) -> Result<crate::native_document_host::HeldAdvisoryLock, String> {
    // Each unlink is one sweep's or release's, once per journal.
    for _ in 0..4 {
        let lock = open_journal_lock(lock_path)
            .map_err(|error| format!("open journal lock {}: {error}", lock_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            lock.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    format!("protect journal lock {}: {error}", lock_path.display())
                })?;
        }
        #[cfg(test)]
        lock_opened_for_test();
        let held = take_journal_lock(lock, path, patience.budget())?;
        if owner::names_file(lock_path, held.file()) {
            return Ok(held);
        }
    }
    Err(format!(
        "journal lock {} was removed under every take; retry",
        lock_path.display()
    ))
}

/// THE DEFECT THESE BUDGETS CLOSE, and the two independent mechanisms that size
/// them.
///
/// A `flock` belongs to the OPEN FILE DESCRIPTION, not to the process or the
/// descriptor, and `fork` hands the child a second descriptor onto the SAME
/// description. So between any child's fork and its `execve`, that child owns a
/// copy of every lock this process held at fork time — the lock stays taken
/// after WE close our descriptor, until the child's `O_CLOEXEC` fires. aterm
/// forks for every shell (`aterm_pty::spawn_shell_with_pid`, a hand-rolled
/// `forkpty` whose child
/// closes only the pty pair and the status pipe before `execve` — every other
/// inherited descriptor, this lock included, rides to the exec), from a process
/// whose document worker takes this lock for every draft append. A single
/// `try_lock` therefore reports `WouldBlock` for a lock nobody wants, which is
/// both a false diagnosis and a LOST APPEND: the reducer takes its failure
/// branch and that document stops journaling.
///
/// A refusal is believed only once it outlasts the budget below. Two DIFFERENT
/// mechanisms hold this lock for a stretch, and both are real — a constant sized
/// for only one of them is a constant nobody can maintain:
///
///  1. FORK RESIDUE — a child of ours that has not reached `execve`. This is the
///     mechanism above and it is self-inflicted: no other process is involved.
///     Its duration is a scheduling latency, measured directly in this repo
///     (`aterm_pty`'s `EXEC_STATUS_BUDGET` doc times fork to the `O_CLOEXEC` status
///     pipe's EOF, which IS this window): p50 2.99 ms, p99 6.0 ms, max 12.7 ms
///     over n=5000 at ordinary load; p50 4.5 ms, p99 206 ms, MAX 523 ms over
///     n=20000 under deliberately pathological load (loadavg 118 on 18 cores).
///     This lock and the sibling save lock are now released through `LOCK_UN`
///     (`native_document_host::HeldAdvisoryLock`), which frees the description
///     the child copied at the holder's release rather than at the child's exec
///     (the product fd-hygiene sweep of 2026-09-27). The budgets are kept for the
///     genuine peers: mechanism 2, and an older aterm sharing the journal that
///     still releases by the close.
///  2. A PEER'S DEVICE-BARRIER HOLD — a legitimate holder inside the locked
///     section, which spans `sync_all` on the temporary (`atomic_replace_locked`)
///     and `sync_directory` on the parent. Both are `F_FULLFSYNC`
///     device-cache barriers on Apple targets, whose latency is bounded by the
///     whole machine's I/O and not by the bytes written. Measured for THIS
///     locked section: 9.13 ms mean / 13.9 ms p99 / 25.5 ms max over n=200 idle,
///     and 20.5 ms mean / 42.9 ms p99 / 69.7 ms max over n=300 at loadavg 17 on
///     18 cores with eight concurrent `fsync` writers. The sibling save path,
///     whose locked section has the same two-barrier shape, was caught at
///     82.5-279.7 ms in six leaked fixtures (`native_document_host`'s
///     `PREFLIGHT_RETRY_BUDGET`).
///
/// So the lock is held until the LATER of those clears, and the worst legitimate
/// hold this repo has ever measured on either mechanism is 523 ms.
///
/// WHAT THIS DOES NOT FIX, stated plainly:
///
///  * A THRESHOLD, NOT A PROOF. Neither budget can distinguish "our own child has
///    not `execve`d yet" from "a peer is mid-barrier" from "another aterm has the
///    journal open"; it only decides how long a refusal must persist before it is
///    believed. The deterministic cure is OWNER IDENTITY — have the holder write
///    its `std::process::id()` into the lock file before publishing and read it on
///    `WouldBlock` (an advisory lock never blocks reads), so self-inflicted residue
///    is ridden out and a live peer is reported busy at once. That is a bigger
///    change than this one and is not taken here.
///  * NOT `fcntl(F_SETLK)`. POSIX record locks are per-PROCESS, so a second request
///    from this process SUCCEEDS instead of contending — that would silently delete
///    the exclusion between the event loop's `initialize` and the worker's
///    `execute_journal_append`, which are genuinely concurrent — and they drop all
///    of a process's locks on ANY `close()` of the file, which `with_journal_lock`
///    performs on every call. `F_OFD_SETLK` keeps the per-description semantics but
///    is inherited by `fork` exactly as `flock` is, so it fixes nothing here.
///  * A GENUINELY CONTENDED OPEN COSTS THE EVENT LOOP ITS WHOLE BUDGET — 25 ms,
///    one and a half frames, once per document open, and only when the lock is
///    actually refused. Past it the open FAILS with the retry guidance rather than
///    waiting longer.
///  * NO `cfg` GATE, deliberately. Windows has no `fork`, so mechanism 1 cannot
///    happen there and only mechanism 2 can hold the lock — but mechanism 1 is the
///    only term that raises the budget, and it raises it solely on `Worker`, the
///    thread that exists to absorb waits. The event loop's budget is the same 25 ms
///    on every platform. A Windows-specific constant would buy nothing and add a
///    second number to keep true.
///  * SCOPED OUT, having been checked: the crate's other single-shot file
///    `try_lock`s. `control_auth.rs`'s instance lease has the same `flock`
///    inheritance but a different contract — it is taken once and held for the whole
///    process lifetime, so a residue window cannot change who owns it, and the only
///    consequence is that `sweep_dead_instance_namespaces_with` may see an abandoned
///    namespace as live for one fork window and sweep it on a later launch.
///    `kitty_log.rs`'s rotation lock is documented best-effort and skips the merge
///    on any refusal, which loses a log line, not a draft. Neither is a lost user
///    edit, which is the harm that justified spending a budget here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JournalLockPatience {
    /// The caller is the thread that owns `App` — the event loop. Every
    /// millisecond spent here is a dropped frame, so this budget is deliberately
    /// in the same class as the sibling write lock's, and for the same reason
    /// spelled out on `native_document_host::WRITE_LOCK_RETRY_BUDGET`, which
    /// [`Self::budget`] returns for it.
    EventLoop,
    /// The caller is the `aterm-native-document` worker (`native_document_queue`),
    /// whose whole job is to keep exactly this work off the event loop. Parking
    /// it costs no frame and drops no input; refusing costs an append.
    Worker,
    /// The launch sweep and a released journal's settle (`owner`): one try. A
    /// refusal means a writer is at this journal now, so it is no candidate
    /// this time, and waiting would only hold up that writer's next take.
    #[cfg_attr(
        not(any(unix, windows)),
        expect(dead_code, reason = "nothing is swept where no image is marked")
    )]
    Sweep,
}

impl JournalLockPatience {
    /// The ceiling this caller can afford. A CEILING, not a floor: an
    /// uncontended take never sleeps at all under either value.
    ///
    /// EVENT LOOP — 25 ms. That is ~1.5 frames at 60 Hz and covers mechanism 1's
    /// ordinary-load distribution (4x its p99, 2x its max) and mechanism 2's
    /// idle distribution (~2x p99). It does NOT cover either tail, and is not
    /// asked to: past it the honest answer is the refusal this module already
    /// contracts for. Matching the sibling constant is deliberate — the same
    /// number for the same thread means the next reader has ONE frame argument to
    /// understand, not two, and the guard test named on `EventLoop` below fails if
    /// the two ever drift apart.
    ///
    /// WORKER — 2 s. Sized from the measurements above, not from headroom under
    /// some test's assertion: ~4x the worst legitimate hold ever measured on
    /// either mechanism (523 ms), ~7x the worst peer barrier hold (279.7 ms),
    /// and enough for the two to overlap and still clear. Validated
    /// empirically as well as arithmetically: at 250 ms four journal tests still
    /// lost appends with the machine at 8x core oversubscription, while 2 s held
    /// across 40 consecutive full-suite runs. The sibling save path spends a
    /// comparable total on this same hazard (25 ms inner x a 500 ms outer
    /// `PREFLIGHT_RETRY_BUDGET` that re-runs the whole side-effect-free preflight,
    /// lock acquisition included); the journal has no outer retry, so its budget
    /// is the whole allowance rather than the inner slice of one.
    const fn budget(self) -> std::time::Duration {
        match self {
            Self::EventLoop => crate::native_document_host::WRITE_LOCK_RETRY_BUDGET,
            Self::Worker => std::time::Duration::from_secs(2),
            Self::Sweep => std::time::Duration::ZERO,
        }
    }
}

/// The journal's rendering of the crate's ONE bounded advisory-lock retry
/// (`native_document_host::acquire_advisory_lock_within` — same loop, same
/// exponential backoff clamped to the remaining budget, same "retry `WouldBlock`
/// only, never `Error`" rule). This function adds nothing but the product
/// vocabulary and the `path` those messages name.
///
/// Takes `budget` rather than reading a constant: the event loop and the worker
/// afford different waits ([`JournalLockPatience`]). The shared loop's ride-out is
/// pinned by `native_document_host`'s
/// `a_transient_save_lock_holder_is_ridden_out_instead_of_refusing_the_save`.
fn take_journal_lock(
    lock: File,
    path: &Path,
    budget: std::time::Duration,
) -> Result<crate::native_document_host::HeldAdvisoryLock, String> {
    crate::native_document_host::acquire_advisory_lock_within(lock, budget).map_err(|refusal| {
        match refusal {
            crate::native_document_host::AdvisoryLockRefusal::Busy => format!(
                "recovery journal {} is busy; retry opening Manual or saving after the \
                 other aterm process finishes",
                path.display()
            ),
            crate::native_document_host::AdvisoryLockRefusal::Failed(error) => {
                format!("lock journal {}: {error}", path.display())
            }
        }
    })
}

/// The sole production journal-image reader. The shared file-feed admission
/// helper opens once with non-blocking/no-follow platform flags, proves that
/// handle regular, rejects metadata oversize early, and reads at most
/// `MAX_JOURNAL_BYTES + 1` so growth during the read cannot allocate unboundedly.
fn read_journal_image(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match aterm_effects::file_feed::read_bounded_regular_file(path, MAX_JOURNAL_BYTES) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Err(std::io::Error::new(
            error.kind(),
            format!("recovery journal exceeds the {MAX_JOURNAL_BYTES}-byte limit"),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            Err(journal_not_regular("image"))
        }
        Err(error) => Err(error),
    }
}

fn read_existing_journal(path: &Path, stage: &str) -> Result<Vec<u8>, String> {
    match read_journal_image(path) {
        Ok(Some(bytes)) => Ok(bytes),
        Ok(None) => Err(format!("{stage}: recovery journal does not exist")),
        Err(error) => Err(format!("{stage}: {error}")),
    }
}

fn observe_journal_image(path: &Path) -> Result<JournalImageGeneration, String> {
    match read_journal_image(path) {
        Ok(Some(bytes)) => Ok(JournalImageGeneration::of(&bytes)),
        Ok(None) => Ok(JournalImageGeneration::missing()),
        Err(error) => Err(format!(
            "could not observe journal {}: {error}",
            path.display()
        )),
    }
}

#[derive(Debug)]
enum JournalReplaceError {
    BeforePublication(std::io::Error),
    PublishedUnverified(std::io::Error),
}

impl std::fmt::Display for JournalReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforePublication(error) => write!(f, "before publication: {error}"),
            Self::PublishedUnverified(error) => write!(
                f,
                "journal replacement is visible but its directory durability is unverified; \
                 reopen/reconcile before retrying: {error}"
            ),
        }
    }
}

/// `owner` is the publishing store's owner identity, named on the image
/// (`owner::mark_publication`); `None` publishes it with its holders unknown.
fn atomic_replace_locked(
    path: &Path,
    bytes: &[u8],
    expected: JournalImageGeneration,
    owner: Option<JournalOwnerId>,
) -> Result<(), JournalReplaceError> {
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(JournalReplaceError::BeforePublication(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("journal exceeds the {MAX_JOURNAL_BYTES}-byte limit"),
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        JournalReplaceError::BeforePublication(std::io::Error::other(
            "journal has no parent directory",
        ))
    })?;
    create_private_dir(parent).map_err(JournalReplaceError::BeforePublication)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("draft.atdj");
    let mut temporary = None;
    for nonce in 1..=10_000_u32 {
        let candidate = parent.join(format!(".{name}.{}-{nonce}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        match options.open(&candidate) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(JournalReplaceError::BeforePublication(error)),
        }
    }
    let (temporary_path, mut temporary_file) = temporary.ok_or_else(|| {
        JournalReplaceError::BeforePublication(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "temp exhausted",
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary_file
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(JournalReplaceError::BeforePublication)?;
    }
    let result = (|| {
        temporary_file
            .write_all(bytes)
            .map_err(JournalReplaceError::BeforePublication)?;
        let mark = owner::mark_publication(
            &temporary_file,
            &temporary_path,
            path,
            expected,
            ContentFingerprint::of(bytes),
            owner,
        );
        crate::durable::sync_all(&temporary_file)
            .map_err(JournalReplaceError::BeforePublication)?;
        drop(temporary_file);
        let before_publication = observe_journal_image(path).map_err(|message| {
            JournalReplaceError::BeforePublication(std::io::Error::other(message))
        })?;
        if before_publication != expected {
            return Err(JournalReplaceError::BeforePublication(
                std::io::Error::other(format!(
                    "journal target changed before publication (expected {expected:?}, found \
                     {before_publication:?})"
                )),
            ));
        }
        replace_file(&temporary_path, path, expected.exists)
            .map_err(JournalReplaceError::BeforePublication)?;
        owner::confirm_publication(path, &mark);
        sync_directory(parent, Flush::Durable).map_err(JournalReplaceError::PublishedUnverified)?;
        let committed = observe_journal_image(path).map_err(|message| {
            JournalReplaceError::PublishedUnverified(std::io::Error::other(message))
        })?;
        let desired = JournalImageGeneration::of(bytes);
        if committed != desired {
            return Err(JournalReplaceError::PublishedUnverified(
                std::io::Error::other(format!(
                    "published journal does not match desired image (expected {desired:?}, found \
                     {committed:?})"
                )),
            ));
        }
        Ok(())
    })();
    if temporary_path.exists() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

#[cfg(test)]
/// Retry an EVENT-LOOP-patience journal call while it reports the product's
/// TRANSIENT contention error.
///
/// NOT a workaround for the engine — a stand-in for the USER. `initialize`
/// runs on the thread that owns `App`, so by contract it spends one frame's
/// budget and then REFUSES with "retry opening Manual" rather than parking the
/// event loop (see [`JournalLockPatience`]). The recovery path for that
/// refusal is a retry, and in a test there is no user to perform it. Under
/// this binary's 3,500-test thread pool plus a process-global document worker
/// that performs real journal rewrites, a fork window landing on a 25 ms
/// budget is ordinary weather.
///
/// It cannot mask the engine fix. The ride-out is pinned by
/// `native_document_host`'s
/// `a_transient_save_lock_holder_is_ridden_out_instead_of_refusing_the_save`,
/// which goes nowhere near this helper.
///
/// Bounded, so a lock that never frees still fails; and only the busy error is
/// retried — every other error returns on the first attempt.
pub(crate) fn settle_busy<T>(mut attempt: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    loop {
        match attempt() {
            Err(error) if error.contains("is busy") && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            settled => return settled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_store::{DocumentStore, TextEdit};
    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt as _;

    fn test_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("aterm-draft-journal-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn snapshots(text: &str) -> (DocumentStore, DocumentId, DocumentSnapshot) {
        let mut store = crate::test_document_store();
        let id = store.open("file:///tmp/draft.md".to_string(), text.to_string());
        let snapshot = store.snapshot(id).unwrap();
        (store, id, snapshot)
    }

    #[cfg(unix)]
    fn make_fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt as _;

        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `path` is a live NUL-terminated pathname and `mkfifo` retains
        // no pointer. Every caller owns its private test directory.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }

    #[test]
    fn journal_reader_accepts_exact_cap_and_rejects_sparse_over_cap() {
        let root = test_root("bounded-reader");
        fs::create_dir_all(&root).unwrap();
        let exact = root.join("exact.atdj");
        File::create(&exact)
            .unwrap()
            .set_len(MAX_JOURNAL_BYTES as u64)
            .unwrap();
        let bytes = read_journal_image(&exact)
            .expect("exact-cap regular journal is admitted")
            .expect("exact-cap journal exists");
        assert_eq!(bytes.len(), MAX_JOURNAL_BYTES);
        drop(bytes);

        let oversized = root.join("oversized.atdj");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_JOURNAL_BYTES as u64 + 1)
            .unwrap();
        let started = std::time::Instant::now();
        let error = read_journal_image(&oversized).expect_err("oversized journal is rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(&MAX_JOURNAL_BYTES.to_string()));
        // A hang detector, never a latency budget: the cap is judged by the error
        // above, and only a reader that never returns is judged by this clock. 1 s
        // crossed under full-suite load (cb8c0cff).
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the over-cap read never returned"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn inspection_refuses_fifo_and_final_symlink_without_waiting() {
        let root = test_root("special-inspection");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let canonical = "file:///tmp/special-inspection.md";
        let path = host.path_for(host.key_for(canonical));
        make_fifo(&path);
        let started = std::time::Instant::now();
        let fifo_error = host.inspect_open(canonical, b"disk").unwrap_err();
        // Refuses-without-BLOCKING: opening the FIFO parks FOREVER (no writer
        // comes), so this is a hang detector, never a latency budget. 1 s crossed
        // under full-suite load (cb8c0cff).
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "inspecting the FIFO journal parked"
        );
        assert!(fifo_error.contains("regular non-link"), "{fifo_error}");

        fs::remove_file(&path).unwrap();
        let victim = root.join("victim.atdj");
        fs::write(&victim, b"do not follow").unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        let link_error = host.inspect_open(canonical, b"disk").unwrap_err();
        assert!(link_error.contains("recovery journal"), "{link_error}");
        assert_eq!(fs::read(&victim).unwrap(), b"do not follow");
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_lock_cannot_park_journal_initialization() {
        let root = test_root("fifo-lock");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("base");
        let canonical = "file:///tmp/fifo-lock.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let lock = journal_lock_path(&decision.path).unwrap();
        make_fifo(&lock);

        let started = std::time::Instant::now();
        let error = host.initialize(decision, &disk, &disk).unwrap_err();
        // Refuses-without-BLOCKING: opening the FIFO lock parks FOREVER, so this is
        // a hang detector, never a latency budget. 1 s crossed under full-suite
        // load (cb8c0cff).
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the FIFO journal lock parked initialization"
        );
        assert!(error.contains("journal lock"), "{error}");
        assert!(fs::symlink_metadata(&lock).unwrap().file_type().is_fifo());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn held_journal_lock_returns_busy_and_a_later_retry_succeeds() {
        let root = test_root("held-lock");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("base");
        let canonical = "file:///tmp/held-lock.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let lock_path = journal_lock_path(&decision.path).unwrap();
        let held = crate::native_document_host::HeldAdvisoryLock::lock(
            open_journal_lock(&lock_path).unwrap(),
        )
        .unwrap();

        let started = std::time::Instant::now();
        let error = host.initialize(decision.clone(), &disk, &disk).unwrap_err();
        // Refuses-without-BLOCKING: a blocking `flock` on a lock nobody releases
        // parks FOREVER, so this is a hang detector, never a latency budget. 1 s
        // crossed under full-suite load (cb8c0cff).
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the held journal lock parked initialization"
        );
        assert!(error.contains("busy"), "{error}");
        assert!(error.contains("retry"), "{error}");

        drop(held);
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();
        assert!(recover_journal_for(initialized.key, &fs::read(initialized.path).unwrap()).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    /// A second descriptor onto the open file description behind the one
    /// descriptor this process holds on `path` — what a child forked at this
    /// instant would hold until it execs. Found by identity, because the
    /// description under test is private to the call that opened it.
    #[cfg(unix)]
    fn copy_of_the_open_description_of(path: &Path) -> File {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        fn identity(fd: libc::c_int) -> Option<(libc::dev_t, libc::ino_t)> {
            // SAFETY: `fstat` only writes the out-parameter, which lives for the
            // call; a closed slot fails with EBADF and is skipped.
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            (unsafe { libc::fstat(fd, &raw mut stat) } == 0).then_some((stat.st_dev, stat.st_ino))
        }

        let wanted = {
            let probe = File::open(path).expect("open the lock to learn its identity");
            identity(probe.as_raw_fd()).expect("fstat the lock")
        };
        for fd in 0..(1 << 16) {
            if identity(fd) == Some(wanted) {
                // SAFETY: `fd` is the live lock descriptor its owner is still
                // holding on this thread's stack; the copy is a fresh descriptor
                // this call owns.
                let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
                assert!(
                    copy >= 0,
                    "dup the lock: {}",
                    std::io::Error::last_os_error()
                );
                return unsafe { File::from_raw_fd(copy) };
            }
        }
        panic!("no open descriptor refers to {}", path.display());
    }

    /// The journal lock is FREE the instant the locked operation returns, even
    /// while a copy of its description lives. The copy stands in for a shell
    /// another thread forked mid-append: a forked child holds every descriptor
    /// until it execs, and a lock released by the close alone lives on in that
    /// copy — so the event loop's 25 ms take after a worker append refused an
    /// open, and the worker waited out a lock nobody held (the product
    /// fd-hygiene sweep of 2026-09-27).
    #[cfg(unix)]
    #[test]
    fn a_released_journal_lock_is_free_while_a_forked_copy_of_it_lives() {
        let root = test_root("forked-copy");
        let path = root.join("draft.atdj");
        let lock_path = journal_lock_path(&path).unwrap();

        let forked_copy = with_journal_lock(&path, JournalLockPatience::Worker, || {
            Ok(copy_of_the_open_description_of(&lock_path))
        })
        .unwrap();

        let next = crate::native_document_host::HeldAdvisoryLock::try_lock(
            open_journal_lock(&lock_path).unwrap(),
        );
        assert!(
            next.is_ok(),
            "the lock outlived its operation on the forked copy of its description"
        );
        drop(forked_copy);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn final_symlink_lock_is_refused_without_touching_its_victim() {
        let root = test_root("symlink-lock");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("base");
        let canonical = "file:///tmp/symlink-lock.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let lock_path = journal_lock_path(&decision.path).unwrap();
        let victim = root.join("lock-victim");
        fs::write(&victim, b"do not lock through me").unwrap();
        std::os::unix::fs::symlink(&victim, &lock_path).unwrap();

        let error = host.initialize(decision, &disk, &disk).unwrap_err();
        assert!(error.contains("journal lock"), "{error}");
        assert_eq!(fs::read(&victim).unwrap(), b"do not lock through me");
        assert!(
            fs::symlink_metadata(&lock_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn journal_root_final_symlink_is_refused_without_chmod_or_writes() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let fixture = test_root("symlink-root");
        let victim = fixture.join("victim");
        let linked_root = fixture.join("drafts");
        fs::create_dir_all(&victim).unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&victim, &linked_root).unwrap();

        let error = DraftJournalHost::new(linked_root.clone()).unwrap_err();
        assert!(
            error.contains("symbolic link")
                || error.contains("Too many levels")
                || error.contains("Not a directory"),
            "{error}"
        );
        assert_eq!(
            fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(fs::read_dir(&victim).unwrap().next().is_none());
        assert!(
            fs::symlink_metadata(&linked_root)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = fs::remove_dir_all(fixture);
    }

    #[cfg(unix)]
    #[test]
    fn hostile_append_and_rewrite_targets_fail_without_mutation() {
        let root = test_root("hostile-persistence");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("base");
        let canonical = "file:///tmp/hostile-persistence.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();
        let original = fs::read(&initialized.path).unwrap();
        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: disk.text.len()..disk.text.len(),
                insert: " changed".to_string(),
            }],
        );
        let current = store.snapshot(id).unwrap();
        let mut reducer = DraftJournalReducer::new_with_key(id, initialized.key, disk.seq);
        let mut append = reducer.plan_snapshot(&current).unwrap();
        append.expected_image = Some(initialized.image_fingerprint);
        let rewrite = checkpoint_plan(
            initialized.key,
            JournalRewriteGeneration(1),
            disk.seq,
            initialized.image_fingerprint,
            &disk,
            &current,
        )
        .unwrap();

        fs::remove_file(&initialized.path).unwrap();
        make_fifo(&initialized.path);
        let started = std::time::Instant::now();
        assert!(matches!(
            execute_journal_append(
                &initialized.path,
                initialized.key,
                &append,
                JournalLockPatience::Worker,
            ),
            JournalAppendResult::Failed { message, .. }
                if message.contains("regular non-link")
        ));
        // Refuses-without-BLOCKING: opening the FIFO journal parks FOREVER, so
        // this is a hang detector, never a latency budget. 1 s crossed under
        // full-suite load (cb8c0cff).
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "appending to the FIFO journal parked"
        );
        assert!(
            fs::symlink_metadata(&initialized.path)
                .unwrap()
                .file_type()
                .is_fifo()
        );

        fs::remove_file(&initialized.path).unwrap();
        let victim = root.join("rewrite-victim.atdj");
        fs::write(&victim, &original).unwrap();
        std::os::unix::fs::symlink(&victim, &initialized.path).unwrap();
        assert!(matches!(
            execute_journal_rewrite(&initialized.path, &rewrite, JournalLockPatience::Worker),
            JournalRewriteResult::Failed(message) if message.contains("preflight")
        ));
        assert_eq!(fs::read(&victim).unwrap(), original);
        assert!(
            fs::symlink_metadata(&initialized.path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clean_recovery_replays_before_reinitializing_sequence_space() {
        let root = test_root("recover");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("base");
        let current = {
            let outcome = store.transact(
                id,
                disk.seq,
                vec![TextEdit {
                    range: 4..4,
                    insert: " draft".to_string(),
                }],
            );
            assert!(matches!(
                outcome,
                crate::document_store::DocumentTxnOutcome::Committed { .. }
            ));
            store.snapshot(id).unwrap()
        };
        let canonical = "file:///tmp/draft.md";
        let first = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        settle_busy(|| host.initialize(first.clone(), &disk, &current)).unwrap();

        let reopened = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        assert_eq!(reopened.recovered_text.as_deref(), Some("base draft"));
        assert!(matches!(reopened.notice, Some(RecoveryNotice::Recovered)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn conflict_and_corruption_preserve_material_fail_closed() {
        let root = test_root("preserve");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("base");
        let current = {
            let _ = store.transact(
                id,
                disk.seq,
                vec![TextEdit {
                    range: 4..4,
                    insert: " draft".to_string(),
                }],
            );
            store.snapshot(id).unwrap()
        };
        let canonical = "file:///tmp/draft.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let initialized =
            settle_busy(|| host.initialize(decision.clone(), &disk, &current)).unwrap();
        let original = fs::read(&initialized.path).unwrap();

        let conflict = host.inspect_open(canonical, b"external").unwrap();
        assert!(matches!(
            conflict.notice,
            Some(RecoveryNotice::DiskConflict)
        ));
        let (other_store, _, external) = snapshots("external");
        let preserved =
            settle_busy(|| host.initialize(conflict.clone(), &external, &external)).unwrap();
        let preserved_copy = |n: u32| {
            let stem = preserved.path.file_stem().unwrap().to_str().unwrap();
            root.join(format!("{stem}.preserved-{n}.atdj"))
        };
        assert_eq!(fs::read(preserved_copy(1)).unwrap(), original);
        drop(other_store);

        fs::write(&preserved.path, b"torn").unwrap();
        let corrupt = host.inspect_open(canonical, b"external").unwrap();
        assert!(matches!(corrupt.notice, Some(RecoveryNotice::Corrupt)));
        let kept = settle_busy(|| host.initialize(corrupt.clone(), &external, &external)).unwrap();
        assert_eq!(fs::read(preserved_copy(2)).unwrap(), b"torn");
        assert_eq!(kept.path, preserved.path);
        let _ = fs::remove_dir_all(root);
    }

    /// Opened and never edited, then changed on disk by someone else (a `git
    /// pull`): the reopen is plain. No draft ever existed, so there is no
    /// conflict to report and no copy to keep, and the one image is replaced
    /// by the file as it is now. A re-seat keeps it so: the first process,
    /// taking the journal back from the second (an update rolled back),
    /// publishes its clean document as one snapshot again, which the next open
    /// over the changed file reads as plain too.
    #[test]
    fn a_clean_journal_reopened_over_a_changed_file_holds_no_draft() {
        let root = test_root("clean-reopen");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let canonical = "file:///tmp/draft.md";
        let (_store, id, v0) = snapshots("v0");
        let opened = journals
            .inspect_open(canonical, v0.text.as_bytes())
            .unwrap();
        settle_busy(|| journals.initialize(opened.clone(), &v0, &v0)).unwrap();

        let host = DraftJournalHost::new(root.clone()).unwrap();
        let reopened = host.inspect_open(canonical, b"v1").unwrap();
        assert_eq!(reopened.notice, None);
        assert_eq!(reopened.recovered_text, None);
        assert!(!reopened.preserve_existing);
        let (_store, _id, v1) = snapshots("v1");
        let initialized = settle_busy(|| host.initialize(reopened.clone(), &v1, &v1)).unwrap();
        assert_eq!(initialized.notice, None);
        assert!(
            preserved_images(&root).is_empty(),
            "a clean image is replaced, not kept aside"
        );
        let recovered =
            recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
        assert_eq!(recovered.text, "v1");

        let (result, _) = run_reseat(&mut journals, id);
        assert!(
            matches!(
                result,
                JournalReseatResult::Republished {
                    preserved: false,
                    ..
                }
            ),
            "{result:?}"
        );
        let recovered =
            recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
        assert_eq!((recovered.text.as_str(), recovered.record_count), ("v0", 1));
        let reopened = host.inspect_open(canonical, b"v1").unwrap();
        assert_eq!(reopened.notice, None);
        assert!(!reopened.preserve_existing);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_open_inspection_cannot_replace_a_newer_initialized_journal() {
        let root = test_root("initialize-cas");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("base");
        let canonical = "file:///tmp/draft.md";
        let first = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let stale = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let initialized = settle_busy(|| host.initialize(first.clone(), &disk, &disk)).unwrap();
        let before = fs::read(&initialized.path).unwrap();

        // Ridden out through `settle_busy` like the first attempt above: the
        // full-suite pool can land this call inside a peer's lock window, which
        // reports the journal BUSY here where the assertion wants the
        // stale-inspection refusal. The helper retries only the busy refusal,
        // so the first real answer is still what the assertion sees.
        let error = settle_busy(|| host.initialize(stale.clone(), &disk, &disk)).unwrap_err();
        assert!(
            error.contains("changed after recovery inspection"),
            "{error}"
        );
        assert_eq!(fs::read(&initialized.path).unwrap(), before);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn journal_directory_and_file_are_private_and_uri_free() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = test_root("permissions");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("private");
        let canonical = "file:///Users//example/Secret%20Draft.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&initialized.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let name = initialized.path.file_name().unwrap().to_string_lossy();
        assert!(!name.contains("Secret"));
        assert!(!name.contains("Draft"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn latest_edit_survives_inflight_append_and_stale_completion() {
        let root = test_root("latest");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("a");
        let decision = journals
            .inspect_open("file:///tmp/draft.md", disk.text.as_bytes())
            .unwrap();
        settle_busy(|| journals.initialize(decision.clone(), &disk, &disk)).unwrap();

        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 1..1,
                insert: "b".into(),
            }],
        );
        let second = store.snapshot(id).unwrap();
        journals.observe_commit(&second).unwrap();
        let JournalEffect::Append {
            path,
            key,
            plan: first,
        } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("expected first append")
        };

        let _ = store.transact(
            id,
            second.seq,
            vec![TextEdit {
                range: 2..2,
                insert: "c".into(),
            }],
        );
        let third = store.snapshot(id).unwrap();
        journals.observe_commit(&third).unwrap();
        assert!(journals.next_effect(id).unwrap().is_none());
        assert_eq!(
            journals.complete_append(
                id,
                crate::native_document_io::JournalGeneration(first.generation.0 + 7),
                JournalAppendResult::Committed(JournalAppendProof {
                    appended_len: first.bytes.len(),
                    encoded_fingerprint: first.encoded_fingerprint,
                    published_image: first.expected_image.unwrap(),
                    file_synced: true,
                    renamed_over_journal: true,
                    directory_synced: true,
                }),
            ),
            JournalCompletion::Stale
        );
        assert!(journals.next_effect(id).unwrap().is_none());

        let committed = execute_journal_append(&path, key, &first, JournalLockPatience::Worker);
        assert_eq!(
            journals.complete_append(id, first.generation, committed),
            JournalCompletion::Durable {
                document: id,
                seq: second.seq
            }
        );
        let JournalEffect::Append {
            path,
            key,
            plan: latest,
        } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("latest desired head must be queued")
        };
        let result = execute_journal_append(&path, key, &latest, JournalLockPatience::Worker);
        let completion = journals.complete_append(id, latest.generation, result.clone());
        assert!(
            matches!(completion, JournalCompletion::Durable { seq, .. } if seq == third.seq),
            "latest append must reduce durable at {:?}; got {completion:?} from {result:?}",
            third.seq
        );
        assert_eq!(journals.durable_seq(id), Some(third.seq));
        assert_eq!(
            recover_journal_for(key, &fs::read(path).unwrap())
                .unwrap()
                .text,
            "abc"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saturated_append_deferral_replans_the_latest_desired_head() {
        let root = test_root("append-deferral");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("a");
        let decision = journals
            .inspect_open("file:///tmp/deferred-draft.md", disk.text.as_bytes())
            .unwrap();
        settle_busy(|| journals.initialize(decision.clone(), &disk, &disk)).unwrap();

        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 1..1,
                insert: "b".into(),
            }],
        );
        let first_desired = store.snapshot(id).unwrap();
        journals.observe_commit(&first_desired).unwrap();
        let JournalEffect::Append { plan: first, .. } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("first append expected")
        };

        // A `try_send(Full)` with no later document event must still leave a
        // plan ready for the capacity-release wake to pump.
        assert!(journals.defer_append(id, first.generation));
        let JournalEffect::Append {
            plan: same_head_retry,
            ..
        } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("deferred append must be replanned without another edit")
        };
        assert_ne!(same_head_retry.generation, first.generation);
        assert_eq!(same_head_retry.target_seq, first_desired.seq);

        // Model another `Full` while a newer edit lands in the UI reducer. The
        // rejected encoded image is not retained; retry replans directly to the
        // store's existing latest snapshot.
        let _ = store.transact(
            id,
            first_desired.seq,
            vec![TextEdit {
                range: 2..2,
                insert: "c".into(),
            }],
        );
        let latest = store.snapshot(id).unwrap();
        journals.observe_commit(&latest).unwrap();
        assert!(journals.defer_append(id, same_head_retry.generation));
        let entry = journals.entries.get(&id).unwrap();
        assert!(entry.append_text.is_none());
        assert!(matches!(
            entry.reducer.phase(),
            crate::native_document_io::JournalPhase::Idle
        ));

        let JournalEffect::Append {
            plan: latest_retry, ..
        } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("deferred append must be replanned")
        };
        assert_ne!(latest_retry.generation, same_head_retry.generation);
        assert_eq!(latest_retry.base_durable, first.base_durable);
        assert_eq!(latest_retry.target_seq, latest.seq);
        assert!(Arc::ptr_eq(
            journals
                .entries
                .get(&id)
                .unwrap()
                .append_text
                .as_ref()
                .unwrap(),
            &latest.text
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saturated_rewrite_deferral_restores_checkpoint_intent() {
        let root = test_root("rewrite-deferral");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("base");
        let decision = journals
            .inspect_open("file:///tmp/deferred-checkpoint.md", disk.text.as_bytes())
            .unwrap();
        settle_busy(|| journals.initialize(decision.clone(), &disk, &disk)).unwrap();
        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: disk.text.len()..disk.text.len(),
                insert: " saved".into(),
            }],
        );
        let saved = store.snapshot(id).unwrap();
        journals.observe_commit(&saved).unwrap();
        journals
            .request_checkpoint(
                DurableCheckpoint {
                    document: id,
                    seq: saved.seq,
                    source: DurableSource::AtomicFile,
                },
                saved.text.clone(),
            )
            .unwrap();

        let JournalEffect::Rewrite { plan: first, .. } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("checkpoint rewrite expected")
        };
        assert!(journals.defer_rewrite(id, first.generation));
        let entry = journals.entries.get(&id).unwrap();
        assert!(entry.rewrite_inflight.is_none());
        assert!(Arc::ptr_eq(
            &entry.pending_checkpoint.as_ref().unwrap().text,
            &saved.text
        ));

        let JournalEffect::Rewrite { plan: retry, .. } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("deferred checkpoint must be replanned")
        };
        assert_ne!(retry.generation, first.generation);
        assert_eq!(retry.base_durable, first.base_durable);
        assert_eq!(retry.target_seq, first.target_seq);
        assert_eq!(retry.fingerprint, first.fingerprint);
        assert_eq!(retry.bytes, first.bytes);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn checkpoint_rewrite_is_proof_gated_and_keeps_newer_draft() {
        let root = test_root("checkpoint");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("base");
        let decision = journals
            .inspect_open("file:///tmp/draft.md", disk.text.as_bytes())
            .unwrap();
        settle_busy(|| journals.initialize(decision.clone(), &disk, &disk)).unwrap();
        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 4..4,
                insert: " saved".into(),
            }],
        );
        let saved = store.snapshot(id).unwrap();
        journals.observe_commit(&saved).unwrap();
        // The file save proves `saved`, while another edit becomes the desired
        // head before the journal checkpoint is dispatched.
        let _ = store.transact(
            id,
            saved.seq,
            vec![TextEdit {
                range: saved.text.len()..saved.text.len(),
                insert: " newer".into(),
            }],
        );
        let newer = store.snapshot(id).unwrap();
        journals.observe_commit(&newer).unwrap();
        assert!(
            journals
                .request_checkpoint(
                    DurableCheckpoint {
                        document: id,
                        seq: saved.seq,
                        source: DurableSource::DraftJournal,
                    },
                    saved.text.clone(),
                )
                .unwrap_err()
                .contains("verified file")
        );
        journals
            .request_checkpoint(
                DurableCheckpoint {
                    document: id,
                    seq: saved.seq,
                    source: DurableSource::AtomicFile,
                },
                saved.text.clone(),
            )
            .unwrap();
        let JournalEffect::Rewrite { path, plan } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("checkpoint rewrite expected")
        };
        let mut bad = match execute_journal_rewrite(&path, &plan, JournalLockPatience::Worker) {
            JournalRewriteResult::Committed(proof) => proof,
            other => panic!("rewrite failed: {other:?}"),
        };
        bad.directory_synced = false;
        assert!(matches!(
            journals.complete_rewrite(id, plan.generation, JournalRewriteResult::Committed(bad)),
            JournalCompletion::Failed { .. }
        ));

        // Retry the checkpoint; no fabricated proof advanced reducer state.
        journals
            .request_checkpoint(
                DurableCheckpoint {
                    document: id,
                    seq: saved.seq,
                    source: DurableSource::AtomicFile,
                },
                saved.text.clone(),
            )
            .unwrap();
        let JournalEffect::Rewrite { path, plan } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("checkpoint retry expected")
        };
        let result = execute_journal_rewrite(&path, &plan, JournalLockPatience::Worker);
        assert_eq!(
            journals.complete_rewrite(id, plan.generation, result),
            JournalCompletion::Durable {
                document: id,
                seq: newer.seq
            }
        );
        let recovered = recover_journal_for(
            journals.key_for("file:///tmp/draft.md"),
            &fs::read(path).unwrap(),
        )
        .unwrap();
        assert_eq!(recovered.text, "base saved newer");
        assert_eq!(
            recovered.base_content,
            ContentFingerprint::of(saved.text.as_bytes())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_checkpoint_rewrite_cannot_erase_a_concurrent_append() {
        let root = test_root("rewrite-cas");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("base");
        let canonical = "file:///tmp/draft.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();
        let rewrite = checkpoint_plan(
            initialized.key,
            JournalRewriteGeneration(1),
            disk.seq,
            initialized.image_fingerprint,
            &disk,
            &disk,
        )
        .unwrap();

        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 4..4,
                insert: " newer".to_string(),
            }],
        );
        let newer = store.snapshot(id).unwrap();
        let mut reducer = DraftJournalReducer::new_with_key(id, initialized.key, disk.seq);
        let mut append = reducer.plan_snapshot(&newer).unwrap();
        append.expected_image = Some(initialized.image_fingerprint);
        assert!(matches!(
            execute_journal_append(
                &initialized.path,
                initialized.key,
                &append,
                JournalLockPatience::Worker,
            ),
            JournalAppendResult::Committed(_)
        ));

        assert!(matches!(
            execute_journal_rewrite(&initialized.path, &rewrite, JournalLockPatience::Worker),
            JournalRewriteResult::Failed(message) if message.contains("image changed")
        ));
        let recovered =
            recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
        assert_eq!(recovered.durable_seq, newer.seq);
        assert_eq!(recovered.text, "base newer");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn same_sequence_different_journal_image_is_rejected() {
        let root = test_root("same-seq-aba");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("base");
        let canonical = "file:///tmp/draft.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();
        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 4..4,
                insert: " ours".to_string(),
            }],
        );
        let mut reducer = DraftJournalReducer::new_with_key(id, initialized.key, disk.seq);
        let mut plan = reducer.plan_snapshot(&store.snapshot(id).unwrap()).unwrap();
        plan.expected_image = Some(initialized.image_fingerprint);

        let other = synthetic_snapshot(id, disk.seq, Arc::from("other"));
        let other_image =
            encode_journal(&[JournalRecord::snapshot_for(initialized.key, &other)]).unwrap();
        fs::write(&initialized.path, &other_image).unwrap();
        assert!(matches!(
            execute_journal_append(
                &initialized.path,
                initialized.key,
                &plan,
                JournalLockPatience::Worker,
            ),
            JournalAppendResult::Failed { message, .. } if message.contains("image changed")
        ));
        assert_eq!(fs::read(&initialized.path).unwrap(), other_image);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cross_process_same_journal_generation_has_one_winner() {
        const CHILD: &str = "ATERM_JOURNAL_COMMIT_TEST_CHILD";
        // Every wait below is for something that must happen — a child test
        // process of this whole binary starting (seconds of exec on a loaded
        // machine), the parent's go, the peer's lock hold ending — so each is
        // a hang detector, never a latency budget (bounds of 10 s and 3 s
        // failed all three cross-process tests together under a 24-way CPU
        // load).
        const HANG: std::time::Duration = std::time::Duration::from_secs(60);
        const URI: &str = "file:///tmp/cross-process-draft.md";
        if let Ok(role) = std::env::var(CHILD) {
            let path = PathBuf::from(std::env::var("ATERM_JOURNAL_COMMIT_TEST_PATH").unwrap());
            let root = path.parent().unwrap();
            let key = DraftJournalHost::new(root.to_path_buf())
                .unwrap()
                .key_for(URI);
            let (mut store, id, disk) = snapshots("base");
            let _ = store.transact(
                id,
                disk.seq,
                vec![TextEdit {
                    range: 4..4,
                    insert: format!(" {role}"),
                }],
            );
            let mut reducer = DraftJournalReducer::new_with_key(id, key, disk.seq);
            let mut plan = reducer.plan_snapshot(&store.snapshot(id).unwrap()).unwrap();
            plan.expected_image = Some(ContentFingerprint::of(&fs::read(&path).unwrap()));
            fs::write(root.join(format!("ready-{role}")), b"ready").unwrap();
            let go = root.join("go");
            let deadline = std::time::Instant::now() + HANG;
            while !go.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent race timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let retry_deadline = std::time::Instant::now() + HANG;
            let verdict = loop {
                match execute_journal_append(&path, key, &plan, JournalLockPatience::Worker) {
                    JournalAppendResult::Committed(_) => break "committed",
                    JournalAppendResult::Failed { message, .. } if message.contains("busy") => {
                        assert!(
                            std::time::Instant::now() < retry_deadline,
                            "journal lock remained busy past the bounded retry window"
                        );
                        std::thread::yield_now();
                    }
                    JournalAppendResult::Failed { message, .. } => {
                        assert!(message.contains("image changed"), "{message}");
                        break "conflict";
                    }
                    JournalAppendResult::Cancelled => panic!("journal child was cancelled"),
                }
            };
            fs::write(root.join(format!("result-{role}")), verdict).unwrap();
            return;
        }

        let root = test_root("process-race");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("base");
        let decision = host.inspect_open(URI, disk.text.as_bytes()).unwrap();
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();
        // Each racer is this very test as a stand-in, which must run exactly
        // this test and pass it.
        let mut children: Vec<_> = ["one", "two"]
            .into_iter()
            .map(|role| {
                let mut child = crate::test_alone::StandIn::this_test();
                child
                    .command()
                    .env(CHILD, role)
                    .env("ATERM_JOURNAL_COMMIT_TEST_PATH", &initialized.path);
                child.spawn()
            })
            .collect();
        crate::test_alone::until_or_ended("both racers reach the barrier", &mut children, || {
            ["one", "two"]
                .iter()
                .all(|role| root.join(format!("ready-{role}")).exists())
        });
        fs::write(root.join("go"), b"go").unwrap();
        for child in children {
            child.passed();
        }
        let results = ["one", "two"]
            .map(|role| fs::read_to_string(root.join(format!("result-{role}"))).unwrap());
        assert_eq!(
            results
                .iter()
                .filter(|result| *result == "committed")
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| *result == "conflict")
                .count(),
            1
        );
        let recovered =
            recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
        assert!(recovered.text == "base one" || recovered.text == "base two");

        // Tier-1 projection: the real losing child is admitted only as the
        // model's disk-generation rejection; accepting it is the negative control.
        let model = aterm_models_native::native_draft_journal_model();
        let edited = model.successors("Edit", &model.init_state())[0].clone();
        let begun = model.successors("BeginJournal", &edited)[0].clone();
        let external = model.successors("ExternalJournalCommit", &begun)[0].clone();
        let rejected = model.successors("RejectJournalDiskConflict", &external)[0].clone();
        assert_eq!(rejected["disk_conflict_rejected"], 1);
        assert!(model.check_invariant("JournalImageCas", &rejected));
        assert!(model.successors("AcceptJournal", &external).is_empty());
        let _ = fs::remove_dir_all(root);
    }

    /// `single_edit` swapped its two `char`-decoding scans for byte scans plus a boundary
    /// snap. The delta it emits feeds the `MAX_INSERT_BYTES` delta-vs-snapshot decision and is
    /// proof-checked against the durable image, so it has to stay bit-identical. Differential
    /// test against the original definition over strings built from 1-, 2-, 3- and 4-byte
    /// scalars, with the divergence deliberately landing mid-sequence at both ends.
    #[test]
    fn single_edit_matches_the_char_scan_definition_on_multi_byte_boundaries() {
        /// The pre-optimisation implementation, verbatim.
        fn reference(before: &str, after: &str) -> JournalEdit {
            let mut prefix = 0;
            for (left, right) in before.chars().zip(after.chars()) {
                if left != right {
                    break;
                }
                prefix += left.len_utf8();
            }
            let mut suffix = 0;
            let before_tail = &before[prefix..];
            let after_tail = &after[prefix..];
            for (left, right) in before_tail.chars().rev().zip(after_tail.chars().rev()) {
                if left != right {
                    break;
                }
                suffix += left.len_utf8();
            }
            JournalEdit {
                range: prefix..before.len().saturating_sub(suffix),
                insert: after[prefix..after.len().saturating_sub(suffix)].to_string(),
            }
        }

        // Same byte length, differing only in a trailing continuation byte, so a byte scan
        // stops INSIDE a scalar and the snap is what recovers the char answer.
        const ALPHABET: [&str; 8] = ["a", "b", "é", "è", "→", "←", "🙂", "🙁"];
        let mut cases: Vec<(String, String)> = vec![
            (String::new(), String::new()),
            (String::new(), "🙂".to_string()),
            ("🙂".to_string(), String::new()),
            ("🙂é→".to_string(), "🙁é→".to_string()),
            ("→é🙂".to_string(), "→é🙁".to_string()),
            ("aéb".to_string(), "aèb".to_string()),
            ("prefix🙂".to_string(), "prefix🙂suffix".to_string()),
            ("🙂tail".to_string(), "🙂more tail".to_string()),
        ];
        // A deterministic LCG walk over the alphabet: every (length, edit-site) combination
        // the two loops can disagree on shows up within a few hundred draws.
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut draw = |modulus: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((seed >> 33) % modulus) as usize
        };
        for _ in 0..512 {
            let len = draw(7);
            let before: Vec<&str> = (0..len).map(|_| ALPHABET[draw(8)]).collect();
            let mut after = before.clone();
            match draw(3) {
                0 if !after.is_empty() => {
                    let at = draw(after.len() as u64);
                    after[at] = ALPHABET[draw(8)];
                }
                1 => {
                    let at = draw(after.len() as u64 + 1);
                    after.insert(at, ALPHABET[draw(8)]);
                }
                _ if !after.is_empty() => {
                    let at = draw(after.len() as u64);
                    after.remove(at);
                }
                _ => {}
            }
            cases.push((before.concat(), after.concat()));
        }

        for (before, after) in cases {
            assert_eq!(
                single_edit(&before, &after),
                reference(&before, &after),
                "single_edit diverged on {before:?} -> {after:?}"
            );
        }
    }

    /// One outgoing store with one journaled edit ("disk" → "disk+"), ready to
    /// meet whatever an update's successor left in its journal.
    fn journaled_edit(
        name: &str,
    ) -> (
        PathBuf,
        DocumentJournalStore,
        DocumentStore,
        DocumentId,
        DocumentSnapshot,
        PathBuf,
        JournalDocumentKey,
    ) {
        let root = test_root(name);
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("disk");
        let decision = journals
            .inspect_open("file:///tmp/draft.md", disk.text.as_bytes())
            .unwrap();
        settle_busy(|| journals.initialize(decision.clone(), &disk, &disk)).unwrap();
        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 4..4,
                insert: "+".into(),
            }],
        );
        journals
            .observe_commit(&store.snapshot(id).unwrap())
            .unwrap();
        let JournalEffect::Append { path, key, plan } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("the edit is appended")
        };
        let result = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
        assert!(matches!(
            journals.complete_append(id, plan.generation, result),
            JournalCompletion::Durable { .. }
        ));
        (root, journals, store, id, disk, path, key)
    }

    fn run_reseat(
        journals: &mut DocumentJournalStore,
        id: DocumentId,
    ) -> (JournalReseatResult, JournalCompletion) {
        assert_eq!(journals.owe_reseat_all(), vec![id]);
        let JournalEffect::Reseat { path, plan } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("an owed re-seat runs before anything else")
        };
        let result = execute_journal_reseat(&path, &plan, JournalLockPatience::Worker);
        let completion = journals.complete_reseat(id, plan.generation, result.clone());
        (result, completion)
    }

    fn preserved_images(root: &Path) -> Vec<Vec<u8>> {
        let mut preserved = fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".preserved-"))
            .map(|entry| fs::read(entry.path()).unwrap())
            .collect::<Vec<_>>();
        preserved.sort();
        preserved
    }

    /// Its own image still on disk: the re-seat touches nothing.
    #[test]
    fn a_reseat_leaves_this_stores_own_image_alone() {
        let (root, mut journals, _store, id, _disk, path, _key) = journaled_edit("reseat-own");
        let before = fs::read(&path).unwrap();
        let (result, completion) = run_reseat(&mut journals, id);
        assert_eq!(result, JournalReseatResult::Unchanged);
        assert_eq!(completion, JournalCompletion::Stale);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!journals.reseat_owed(id));
        let _ = fs::remove_dir_all(root);
    }

    /// A successor republished the image (its own sequence numbers, the same
    /// draft): the re-seat puts back `[baseline, head]` under this store's
    /// sequences, preserves nothing (it held nothing new), and the next append
    /// lands where the successor's image made every append fail.
    #[test]
    fn a_reseat_takes_back_a_successors_republication() {
        let (root, mut journals, mut store, id, disk, path, key) =
            journaled_edit("reseat-successor");
        let head = store.snapshot(id).unwrap();
        // The successor's restore: a fresh store over the same file replays the
        // draft and republishes it.
        let mut successor = DocumentJournalStore::for_test(root.clone()).unwrap();
        let (mut theirs, their_id, their_disk) = snapshots("disk");
        let decision = successor
            .inspect_open("file:///tmp/draft.md", their_disk.text.as_bytes())
            .unwrap();
        assert_eq!(decision.recovered_text.as_deref(), Some("disk+"));
        let _ = theirs.transact(
            their_id,
            their_disk.seq,
            vec![TextEdit {
                range: 0..4,
                insert: "disk+".into(),
            }],
        );
        let their_current = theirs.snapshot(their_id).unwrap();
        settle_busy(|| successor.initialize(decision.clone(), &their_disk, &their_current))
            .unwrap();
        let republished = fs::read(&path).unwrap();

        // The outgoing store's next append now fails against that image…
        let _ = store.transact(
            id,
            head.seq,
            vec![TextEdit {
                range: 5..5,
                insert: "!".into(),
            }],
        );
        journals
            .observe_commit(&store.snapshot(id).unwrap())
            .unwrap();
        let JournalEffect::Append { plan, .. } = journals.next_effect(id).unwrap().unwrap() else {
            panic!("an append")
        };
        let refused = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
        assert!(matches!(
            journals.complete_append(id, plan.generation, refused),
            JournalCompletion::Failed { .. }
        ));
        assert_eq!(fs::read(&path).unwrap(), republished, "and changes nothing");

        // …and the re-seat takes it back.
        let (result, completion) = run_reseat(&mut journals, id);
        let JournalReseatResult::Republished {
            baseline,
            preserved,
            ..
        } = result
        else {
            panic!("the successor's image is replaced: {result:?}")
        };
        assert!(!preserved, "the successor's image held nothing new");
        assert_eq!(baseline, ContentFingerprint::of(disk.text.as_bytes()));
        let latest = store.snapshot(id).unwrap();
        assert_eq!(
            completion,
            JournalCompletion::Durable {
                document: id,
                seq: latest.seq
            }
        );
        let recovered = recover_journal_for(key, &fs::read(&path).unwrap()).unwrap();
        assert_eq!(recovered.text, "disk+!", "the latest edit is durable");
        assert_eq!(
            recovered.base_content,
            ContentFingerprint::of(b"disk"),
            "over the file it was made against"
        );
        assert!(preserved_images(&root).is_empty());

        // The journal writes again.
        let _ = store.transact(
            id,
            latest.seq,
            vec![TextEdit {
                range: 6..6,
                insert: "?".into(),
            }],
        );
        journals
            .observe_commit(&store.snapshot(id).unwrap())
            .unwrap();
        let JournalEffect::Append { plan, .. } = journals.next_effect(id).unwrap().unwrap() else {
            panic!("an append")
        };
        let landed = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
        assert!(matches!(
            journals.complete_append(id, plan.generation, landed),
            JournalCompletion::Durable { .. }
        ));
        let _ = fs::remove_dir_all(root);
    }

    /// An image holding a draft this store never had is preserved aside before
    /// the re-seat replaces it; one whose baseline is not this store's leaves
    /// the target as its own baseline, a draft to every later open and re-seat.
    /// Nothing is deleted on any path.
    #[test]
    fn a_reseat_preserves_what_it_never_had() {
        let (root, mut journals, store, id, _disk, path, key) = journaled_edit("reseat-foreign");
        let head = store.snapshot(id).unwrap();
        let foreign = encode_journal(&[
            JournalRecord::snapshot_for(key, &synthetic_snapshot(id, Seq(1), Arc::from("disk"))),
            JournalRecord::snapshot_for(
                key,
                &synthetic_snapshot(id, Seq(2), Arc::from("someone else's draft")),
            ),
        ])
        .unwrap();
        fs::write(&path, &foreign).unwrap();
        let recording = aterm_durable::seam::Recording::start();
        let (result, _) = run_reseat(&mut journals, id);
        #[cfg(unix)]
        let expected_reseat = [Flush::Ordered, Flush::Durable, Flush::Durable];
        #[cfg(not(unix))]
        let expected_reseat = [Flush::Durable];
        assert_eq!(
            recording.take(),
            expected_reseat,
            "the re-seat's file is durable; Unix also flushes the set-aside link and publication"
        );
        drop(recording);
        assert!(
            matches!(
                result,
                JournalReseatResult::Republished {
                    preserved: true,
                    ..
                }
            ),
            "{result:?}"
        );
        assert_eq!(preserved_images(&root), vec![foreign]);
        let recovered = recover_journal_for(key, &fs::read(&path).unwrap()).unwrap();
        assert_eq!(recovered.text, *head.text);
        assert_eq!(recovered.base_content, ContentFingerprint::of(b"disk"));

        // A baseline that is not this store's (another process opened the file
        // as it now is): the target is the baseline now, and the image (which
        // holds no draft of its own) is not preserved. That draft is still a
        // draft to the next open — a conflict kept aside, never a clean image
        // replaced — and stays one through a second round with no edit between.
        let other_base = encode_journal(&[JournalRecord::snapshot_for(
            key,
            &synthetic_snapshot(id, Seq(1), Arc::from("changed on disk")),
        )])
        .unwrap();
        for round in 1..=2 {
            fs::write(&path, &other_base).unwrap();
            let (result, _) = run_reseat(&mut journals, id);
            let JournalReseatResult::Republished {
                baseline,
                preserved,
                ..
            } = result
            else {
                panic!("round {round}: {result:?}")
            };
            assert!(!preserved, "round {round}");
            assert_eq!(baseline, ContentFingerprint::of(head.text.as_bytes()));
            let reopened = journals
                .inspect_open("file:///tmp/draft.md", b"changed on disk")
                .unwrap();
            assert_eq!(
                reopened.notice,
                Some(RecoveryNotice::DiskConflict),
                "round {round}"
            );
            assert!(reopened.preserve_existing, "round {round}");
        }
        assert_eq!(preserved_images(&root).len(), 1);

        // A draft another process re-seated that way — its text twice, its own
        // baseline — is a draft all the same: kept aside before it is replaced.
        let reseated = JournalRecord::snapshot_for(
            key,
            &synthetic_snapshot(id, Seq(3), Arc::from("their re-seated draft")),
        );
        let theirs = encode_journal(&[reseated.clone(), reseated]).unwrap();
        fs::write(&path, &theirs).unwrap();
        let (result, _) = run_reseat(&mut journals, id);
        assert!(
            matches!(
                result,
                JournalReseatResult::Republished {
                    preserved: true,
                    ..
                }
            ),
            "{result:?}"
        );
        let preserved = preserved_images(&root);
        assert_eq!(preserved.len(), 2);
        assert!(preserved.contains(&theirs));
        let _ = fs::remove_dir_all(root);
    }

    /// A save after a re-seat that recorded its draft as its own baseline makes
    /// the baseline the file again: a later clean re-seat writes one snapshot,
    /// and an open over newer contents reads it as clean (DJ1,
    /// docs/AUDIT-persisted-state-2026-10-07.md).
    #[test]
    fn a_save_after_a_reseat_makes_the_baseline_the_file_again() {
        let (root, mut journals, store, id, _disk, path, key) = journaled_edit("save-after-reseat");
        let head = store.snapshot(id).unwrap();
        let other_base = encode_journal(&[JournalRecord::snapshot_for(
            key,
            &synthetic_snapshot(id, Seq(1), Arc::from("changed on disk")),
        )])
        .unwrap();
        fs::write(&path, &other_base).unwrap();
        let _ = run_reseat(&mut journals, id);
        journals
            .request_checkpoint(
                DurableCheckpoint {
                    document: id,
                    seq: head.seq,
                    source: DurableSource::AtomicFile,
                },
                head.text.clone(),
            )
            .unwrap();
        let JournalEffect::Rewrite { path: rpath, plan } =
            journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("the save checkpoints the journal")
        };
        let result = execute_journal_rewrite(&rpath, &plan, JournalLockPatience::Worker);
        assert!(matches!(
            journals.complete_rewrite(id, plan.generation, result),
            JournalCompletion::Durable { .. }
        ));
        let theirs = encode_journal(&[JournalRecord::snapshot_for(
            key,
            &synthetic_snapshot(id, Seq(1), head.text.clone()),
        )])
        .unwrap();
        fs::write(&path, &theirs).unwrap();
        let _ = run_reseat(&mut journals, id);
        let recovered = recover_journal_for(key, &fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            recovered.record_count, 1,
            "a clean re-seat writes one snapshot"
        );
        let reopened = journals
            .inspect_open("file:///tmp/draft.md", b"pulled")
            .unwrap();
        assert_eq!(reopened.notice, None, "a saved document holds no draft");
        let _ = fs::remove_dir_all(root);
    }

    /// While the fence stands (an update attempt is in flight) an owed re-seat
    /// is not planned, and neither is anything behind it.
    #[test]
    fn a_fenced_reseat_holds_every_effect() {
        let (root, mut journals, mut store, id, _disk, _path, _key) =
            journaled_edit("reseat-fenced");
        let head = store.snapshot(id).unwrap();
        journals.owe_reseat_all();
        journals.set_reseat_fenced(true);
        let _ = store.transact(
            id,
            head.seq,
            vec![TextEdit {
                range: 0..0,
                insert: ">".into(),
            }],
        );
        journals
            .observe_commit(&store.snapshot(id).unwrap())
            .unwrap();
        assert!(journals.next_effect(id).unwrap().is_none());
        assert_eq!(
            journals
                .carry_candidate(&store.snapshot(id).unwrap())
                .unwrap_err(),
            DraftCarryRefusal::Reseating
        );
        journals.set_reseat_fenced(false);
        assert!(matches!(
            journals.next_effect(id).unwrap(),
            Some(JournalEffect::Reseat { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }

    /// A re-seat that fails (here: the journal lock held by a peer past the
    /// event loop's budget) is owed again and SAID — the draft is refused by
    /// name, not waited on — until a retry lands.
    #[test]
    fn a_failed_reseat_is_refused_by_name_until_a_retry_lands() {
        let (root, mut journals, store, id, _disk, path, _key) = journaled_edit("reseat-failed");
        let head = store.snapshot(id).unwrap();
        journals.owe_reseat_all();
        let JournalEffect::Reseat { plan, .. } = journals.next_effect(id).unwrap().unwrap() else {
            panic!("a re-seat")
        };
        let held = hold_journal_lock_for_test(&path);
        let refused = execute_journal_reseat(&path, &plan, JournalLockPatience::EventLoop);
        assert!(
            matches!(refused, JournalReseatResult::Failed(_)),
            "{refused:?}"
        );
        assert!(matches!(
            journals.complete_reseat(id, plan.generation, refused),
            JournalCompletion::Failed { .. }
        ));
        assert_eq!(
            journals.carry_candidate(&head).unwrap_err(),
            DraftCarryRefusal::ReseatFailed
        );
        // The retry on the worker still reads as the failure it is retrying.
        let JournalEffect::Reseat { plan, .. } = journals.next_effect(id).unwrap().unwrap() else {
            panic!("the re-seat is owed again")
        };
        assert_eq!(
            journals.carry_candidate(&head).unwrap_err(),
            DraftCarryRefusal::ReseatFailed
        );
        drop(held);
        let landed = execute_journal_reseat(&path, &plan, JournalLockPatience::Worker);
        assert_eq!(landed, JournalReseatResult::Unchanged);
        journals.complete_reseat(id, plan.generation, landed);
        assert!(journals.carry_candidate(&head).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    /// Open `uri` (empty on disk) in `journals` and journal `draft` as its
    /// unsaved head, durably: what a crash or an update leaves behind.
    fn journal_a_draft(
        journals: &mut DocumentJournalStore,
        store: &mut DocumentStore,
        uri: &str,
        draft: &str,
    ) -> PathBuf {
        let id = store.open(uri.to_string(), String::new());
        let disk = store.snapshot(id).unwrap();
        let decision = journals.inspect_open(uri, b"").unwrap();
        assert_eq!(decision.recovered_text, None, "{uri} starts with no draft");
        settle_busy(|| journals.initialize(decision.clone(), &disk, &disk)).unwrap();
        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 0..0,
                insert: draft.to_string(),
            }],
        );
        journals
            .observe_commit(&store.snapshot(id).unwrap())
            .unwrap();
        let JournalEffect::Append { path, key, plan } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("the draft is appended")
        };
        let result = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
        assert!(matches!(
            journals.complete_append(id, plan.generation, result),
            JournalCompletion::Durable { .. }
        ));
        path
    }

    /// hashclock-2 (2026-10-10): two names in one known directory whose
    /// unkeyed FNV-1a/64 identities are equal — the pair a refuter's rho
    /// search found in 45 s — name two journals, and neither document
    /// recovers the other's draft. Both files are empty, so a shared journal's
    /// baseline would match either one's disk and replay silently.
    #[test]
    fn colliding_legacy_names_never_share_a_journal() {
        const A: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
        const B: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
        // The pair collides under the unkeyed identity origin/main named
        // journals by (the refuter's key).
        assert_eq!(
            JournalDocumentKey::legacy_unkeyed(A),
            JournalDocumentKey(0x0307_5386_33bd_1a46)
        );
        assert_eq!(
            JournalDocumentKey::legacy_unkeyed(B),
            JournalDocumentKey::legacy_unkeyed(A)
        );
        let root = test_root("legacy-collision");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let mut store = crate::test_document_store();
        let a_path = journal_a_draft(&mut journals, &mut store, A, "A's unsaved draft");
        // The process ends with A's draft journaled; the next one opens B first.
        drop(journals);
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let opened_b = journals.inspect_open(B, b"").unwrap();
        assert_ne!(opened_b.path, a_path, "B must not name A's journal");
        assert_eq!(
            opened_b.recovered_text, None,
            "B must not recover A's draft"
        );
        assert_eq!(opened_b.notice, None);
        let mut store = crate::test_document_store();
        let b_path = journal_a_draft(&mut journals, &mut store, B, "B's unsaved draft");
        assert_ne!(b_path, a_path);
        drop(journals);
        let journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let opened_a = journals.inspect_open(A, b"").unwrap();
        assert_eq!(opened_a.path, a_path);
        assert_eq!(
            opened_a.recovered_text.as_deref(),
            Some("A's unsaved draft")
        );
        let opened_b = journals.inspect_open(B, b"").unwrap();
        assert_eq!(opened_b.path, b_path);
        assert_eq!(
            opened_b.recovered_text.as_deref(),
            Some("B's unsaved draft")
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Write `uri`'s journal as an aterm from before 2026-10-10 did: under the
    /// unkeyed name, every record carrying the unkeyed key — the file's
    /// snapshot, then the draft as an appended delta.
    fn write_legacy_journal(root: &Path, uri: &str, disk: &str, draft: &str) -> PathBuf {
        let typed: &[&str] = if draft == disk { &[] } else { &[draft] };
        write_legacy_journal_typing(root, uri, disk, typed)
    }

    /// [`write_legacy_journal`] for a draft typed as `texts`, one appended
    /// delta to each in turn.
    fn write_legacy_journal_typing(root: &Path, uri: &str, disk: &str, texts: &[&str]) -> PathBuf {
        let key = JournalDocumentKey::legacy_unkeyed(uri);
        let mut store = crate::test_document_store();
        let id = store.open(uri.to_string(), disk.to_string());
        let base = store.snapshot(id).unwrap();
        let mut records = vec![JournalRecord::snapshot_for(key, &base)];
        let (mut seq, mut text) = (base.seq, disk);
        for &next in texts {
            let after = Seq(seq.0 + 1);
            records.push(
                JournalRecord::delta_for(key, seq, after, vec![single_edit(text, next)]).unwrap(),
            );
            (seq, text) = (after, next);
        }
        let path = root.join(format!("{:016x}.atdj", key.0));
        fs::write(&path, encode_journal(&records).unwrap()).unwrap();
        path
    }

    /// Open `uri` over `disk` as the App does: inspect, install any recovered
    /// draft, initialize. Returns the decision and the document's head.
    fn open_as_the_app_does(
        journals: &mut DocumentJournalStore,
        store: &mut DocumentStore,
        uri: &str,
        disk: &str,
    ) -> (JournalOpenDecision, DocumentId, InitializedJournal) {
        let id = store.open(uri.to_string(), disk.to_string());
        let base = store.snapshot(id).unwrap();
        let decision = journals.inspect_open(uri, disk.as_bytes()).unwrap();
        if let Some(recovered) = decision.recovered_text.clone() {
            let _ = store.transact(
                id,
                base.seq,
                vec![TextEdit {
                    range: 0..base.text.len(),
                    insert: recovered,
                }],
            );
        }
        let current = store.snapshot(id).unwrap();
        let initialized =
            settle_busy(|| journals.initialize(decision.clone(), &base, &current)).unwrap();
        (decision, id, initialized)
    }

    fn type_and_append(
        journals: &mut DocumentJournalStore,
        store: &mut DocumentStore,
        id: DocumentId,
        typed: &str,
    ) -> (JournalAppendPlan, JournalCompletion) {
        let head = store.snapshot(id).unwrap();
        let _ = store.transact(
            id,
            head.seq,
            vec![TextEdit {
                range: head.text.len()..head.text.len(),
                insert: typed.to_string(),
            }],
        );
        journals
            .observe_commit(&store.snapshot(id).unwrap())
            .unwrap();
        let JournalEffect::Append { path, key, plan } = journals.next_effect(id).unwrap().unwrap()
        else {
            panic!("the edit is appended")
        };
        let result = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
        let completion = journals.complete_append(id, plan.generation, result);
        (plan, completion)
    }

    /// MIGRATION (hashclock-2, 2026-10-10): a draft an older aterm journaled
    /// under the unkeyed name is recovered after the upgrade and moved to the
    /// keyed journal. Its legacy file stays — still replaying under its own key,
    /// which is what an outgoing older aterm's update Commit check reads — until
    /// the keyed journal's first write, which retires it first.
    #[test]
    fn a_legacy_journal_is_recovered_and_retired_by_the_first_write() {
        const URI: &str = "file:///tmp/legacy-migration.md";
        let root = test_root("legacy-migration");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let legacy = write_legacy_journal(&root, URI, "disk", "disk and a draft");
        let mut store = crate::test_document_store();
        let (decision, id, initialized) =
            open_as_the_app_does(&mut journals, &mut store, URI, "disk");
        assert_eq!(decision.recovered_text.as_deref(), Some("disk and a draft"));
        assert_eq!(initialized.notice, Some(RecoveryNotice::Recovered));
        assert_ne!(initialized.path, legacy);
        let keyed =
            recover_journal_for(journals.key_for(URI), &fs::read(&initialized.path).unwrap())
                .unwrap();
        assert_eq!(keyed.text, "disk and a draft");
        assert_eq!(keyed.base_content, ContentFingerprint::of(b"disk"));
        verify_carried_draft(
            &DraftCarryCandidate {
                path: legacy.clone(),
                key: JournalDocumentKey::legacy_unkeyed(URI),
            },
            "disk and a draft",
            ContentFingerprint::of(b"disk"),
        )
        .expect("an outgoing older aterm still finds its carried draft");

        let (plan, completion) = type_and_append(&mut journals, &mut store, id, "!");
        assert!(plan.retire_legacy.is_some(), "the first write owes it");
        assert!(matches!(completion, JournalCompletion::Durable { .. }));
        assert!(
            !legacy.exists(),
            "retired before the keyed journal moved on"
        );
        let (plan, completion) = type_and_append(&mut journals, &mut store, id, "?");
        assert_eq!(plan.retire_legacy, None, "and owed no more");
        assert!(matches!(completion, JournalCompletion::Durable { .. }));

        drop(journals);
        let journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let reopened = journals.inspect_open(URI, b"disk").unwrap();
        assert_eq!(
            reopened.recovered_text.as_deref(),
            Some("disk and a draft!?")
        );
        assert_eq!(preserved_images(&root), Vec::<Vec<u8>>::new());
        let _ = fs::remove_dir_all(root);
    }

    /// An update from an older aterm rolled back after its successor migrated
    /// the draft, and the older aterm journaled on: its legacy journal is the
    /// newer and wins the next open; the keyed image the successor left, which
    /// holds a draft the legacy one does not, is set aside, not replaced.
    #[test]
    fn a_legacy_journal_rewritten_after_a_migration_wins_and_the_keyed_one_is_kept() {
        const URI: &str = "file:///tmp/legacy-rollback.md";
        let root = test_root("legacy-rollback");
        let legacy = write_legacy_journal(&root_with_dir(&root), URI, "disk", "disk v1");
        let mut successor = DocumentJournalStore::for_test(root.clone()).unwrap();
        let mut store = crate::test_document_store();
        let (_, _, first) = open_as_the_app_does(&mut successor, &mut store, URI, "disk");
        let migrated = fs::read(&first.path).unwrap();
        drop(successor);
        // The older aterm, owning the document again, journals on.
        write_legacy_journal(&root, URI, "disk", "disk v2");

        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let mut store = crate::test_document_store();
        let (decision, _, _) = open_as_the_app_does(&mut journals, &mut store, URI, "disk");
        assert_eq!(decision.recovered_text.as_deref(), Some("disk v2"));
        assert_eq!(preserved_images(&root), vec![migrated]);
        assert!(legacy.exists(), "a carried draft's legacy file stays");
        let _ = fs::remove_dir_all(root);
    }

    fn root_with_dir(root: &Path) -> PathBuf {
        fs::create_dir_all(root).unwrap();
        root.to_path_buf()
    }

    /// A legacy journal an older aterm wrote again after this one read it is
    /// newer than the migration: the write that owes its retirement refuses,
    /// and the legacy image is left whole for the next open.
    #[test]
    fn a_legacy_journal_changed_after_the_read_refuses_the_retiring_write() {
        const URI: &str = "file:///tmp/legacy-changed.md";
        let root = test_root("legacy-changed");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let legacy = write_legacy_journal(&root, URI, "disk", "disk v1");
        let mut store = crate::test_document_store();
        let (_, id, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, "disk");
        let keyed = fs::read(&initialized.path).unwrap();
        write_legacy_journal(&root, URI, "disk", "disk v2");
        let newer = fs::read(&legacy).unwrap();

        let (_, completion) = type_and_append(&mut journals, &mut store, id, "!");
        let JournalCompletion::Failed { message, .. } = completion else {
            panic!("the retiring write refuses: {completion:?}")
        };
        assert!(message.contains("changed after it was read"), "{message}");
        assert_eq!(fs::read(&legacy).unwrap(), newer);
        assert_eq!(fs::read(&initialized.path).unwrap(), keyed);
        let _ = fs::remove_dir_all(root);
    }

    /// A legacy journal with nothing to carry goes at the open: a clean one
    /// outright, one whose draft conflicts with the file set aside first.
    #[test]
    fn a_legacy_journal_with_nothing_to_carry_is_retired_at_the_open() {
        const CLEAN: &str = "file:///tmp/legacy-clean.md";
        const CONFLICT: &str = "file:///tmp/legacy-conflict.md";
        let root = test_root("legacy-retired-at-open");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let clean = write_legacy_journal(&root, CLEAN, "disk", "disk");
        let conflict = write_legacy_journal(&root, CONFLICT, "old disk", "old disk and a draft");
        let conflict_image = fs::read(&conflict).unwrap();
        let mut store = crate::test_document_store();

        // aterm-durable's counting seam: the keyed image's publish stays durable
        // (file, entry); the legacy journal's retirement and a set-aside link are only
        // ordered, since a lost unlink leaves it to be retired again at the next open
        // and a lost link leaves the image under its own name.
        let recording = aterm_durable::seam::Recording::start();
        let (decision, _, initialized) =
            open_as_the_app_does(&mut journals, &mut store, CLEAN, "disk");
        #[cfg(unix)]
        let expected_clean = [Flush::Durable, Flush::Durable, Flush::Ordered];
        #[cfg(not(unix))]
        let expected_clean = [Flush::Durable];
        assert_eq!(
            recording.take(),
            expected_clean,
            "the keyed image; Unix also flushes publication and legacy retirement"
        );
        assert_eq!(decision.recovered_text, None);
        assert_eq!(initialized.notice, None);
        assert!(!clean.exists());
        assert_eq!(preserved_images(&root), Vec::<Vec<u8>>::new());

        let (decision, _, initialized) =
            open_as_the_app_does(&mut journals, &mut store, CONFLICT, "new disk");
        #[cfg(unix)]
        let expected_conflict = [
            Flush::Durable,
            Flush::Durable,
            Flush::Ordered,
            Flush::Ordered,
        ];
        #[cfg(not(unix))]
        let expected_conflict = [Flush::Durable];
        assert_eq!(
            recording.take(),
            expected_conflict,
            "the keyed image; Unix also flushes publication, set-aside and retirement"
        );
        drop(recording);
        assert_eq!(decision.recovered_text, None);
        assert_eq!(initialized.notice, Some(RecoveryNotice::DiskConflict));
        assert!(!conflict.exists());
        assert_eq!(preserved_images(&root), vec![conflict_image]);
        let _ = fs::remove_dir_all(root);
    }

    /// Review, 2026-10-10: an older aterm that runs after this one (another
    /// build's instance, a downgrade, a rollback release) cannot see a keyed
    /// journal, so opening the file it writes a CLEAN legacy image, the file
    /// alone. That image carries nothing and never hides the keyed draft: the
    /// next open here recovers the draft and retires the legacy image. Beside
    /// a keyed image that does not decode, a clean legacy one lets the open say
    /// so, as it does when no legacy image is there.
    #[test]
    fn a_clean_legacy_journal_never_hides_a_keyed_draft() {
        const URI: &str = "file:///tmp/legacy-clean-after-keyed.md";
        const TORN: &str = "file:///tmp/legacy-clean-after-torn.md";
        let root = test_root("legacy-clean-after-keyed");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let mut store = crate::test_document_store();
        let keyed = journal_a_draft(&mut journals, &mut store, URI, "the newest draft");
        drop(journals);
        // The older aterm opens the file (empty on disk) and journals it clean.
        let legacy = write_legacy_journal(&root, URI, "", "");

        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let mut store = crate::test_document_store();
        let (decision, _, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, "");
        assert_eq!(
            decision.recovered_text.as_deref(),
            Some("the newest draft"),
            "a clean legacy image hid the keyed draft (notice {:?})",
            initialized.notice
        );
        assert_eq!(initialized.notice, Some(RecoveryNotice::Recovered));
        assert_eq!(initialized.path, keyed);
        assert!(
            !legacy.exists(),
            "a clean legacy image is retired at the open"
        );
        assert_eq!(preserved_images(&root), Vec::<Vec<u8>>::new());

        let torn = journals.inspect_open(TORN, b"").unwrap().path;
        fs::write(&torn, b"torn").unwrap();
        let torn_legacy = write_legacy_journal(&root, TORN, "", "");
        let (decision, _, initialized) = open_as_the_app_does(&mut journals, &mut store, TORN, "");
        assert_eq!(decision.recovered_text, None);
        assert_eq!(initialized.notice, Some(RecoveryNotice::Corrupt));
        assert!(!torn_legacy.exists());
        assert_eq!(preserved_images(&root), vec![b"torn".to_vec()]);
        let _ = fs::remove_dir_all(root);
    }

    /// Review, 2026-10-10: a legacy draft that is the file itself (an older
    /// aterm typed, then undid) still wins. Its document is dirty there, so an
    /// outgoing older aterm's update Commit check reads it back, and it stays
    /// until the first write. The keyed draft it sets aside is kept, and the
    /// open SAYS so: a set-aside is never silent.
    #[test]
    fn a_legacy_draft_that_is_the_file_never_sets_a_keyed_draft_aside_silently() {
        const URI: &str = "file:///tmp/legacy-undone-after-keyed.md";
        let root = test_root("legacy-undone-after-keyed");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let mut store = crate::test_document_store();
        let keyed = journal_a_draft(&mut journals, &mut store, URI, "the keyed draft");
        let keyed_image = fs::read(&keyed).unwrap();
        drop(journals);
        let legacy = write_legacy_journal_typing(&root, URI, "", &["typed", ""]);

        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let mut store = crate::test_document_store();
        let (decision, _, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, "");
        assert_eq!(decision.recovered_text, None);
        assert_eq!(
            initialized.notice,
            Some(RecoveryNotice::DiskConflict),
            "the keyed draft was set aside with no notice"
        );
        assert_eq!(preserved_images(&root), vec![keyed_image]);
        assert!(
            legacy.exists(),
            "its draft is carried: it stays until the first write"
        );
        verify_carried_draft(
            &DraftCarryCandidate {
                path: legacy,
                key: JournalDocumentKey::legacy_unkeyed(URI),
            },
            "",
            ContentFingerprint::of(b""),
        )
        .expect("an outgoing older aterm still finds its carried draft");
        let _ = fs::remove_dir_all(root);
    }

    /// The secret is minted once, private and exact, and every store over the
    /// directory reads the same one back; another directory's secret keys the
    /// same URI elsewhere.
    #[test]
    fn the_journal_secret_is_minted_private_once_and_read_back() {
        const URI: &str = "file:///tmp/secret.md";
        let root = test_root("secret-minted");
        let recording = aterm_durable::seam::Recording::start();
        let first = DocumentJournalStore::for_test(root.clone()).unwrap();
        #[cfg(unix)]
        let expected_mint = [Flush::Ordered, Flush::Ordered];
        #[cfg(not(unix))]
        let expected_mint = [Flush::Ordered];
        assert_eq!(
            recording.take(),
            expected_mint,
            "the minted secret is ordered; Unix also flushes its entry"
        );
        let secret = root.join(JOURNAL_SECRET_FILE);
        let image = fs::read(&secret).unwrap();
        assert_eq!(image.len(), JOURNAL_SECRET_FILE_LEN);
        assert_eq!(image[..JOURNAL_SECRET_MAGIC.len()], JOURNAL_SECRET_MAGIC);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let second = DocumentJournalStore::for_test(root.clone()).unwrap();
        assert_eq!(recording.take(), [], "a secret read back writes nothing");
        drop(recording);
        assert_eq!(
            fs::read(&secret).unwrap(),
            image,
            "read back, not re-minted"
        );
        assert_eq!(second.key_for(URI), first.key_for(URI));
        assert_eq!(
            fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .count(),
            0,
            "no temporary is left behind"
        );
        let elsewhere = test_root("secret-elsewhere");
        let other = DocumentJournalStore::for_test(elsewhere.clone()).unwrap();
        assert_ne!(other.key_for(URI), first.key_for(URI));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(elsewhere);
    }

    /// A secret that cannot be used refuses the store, and nothing is minted
    /// over it: the secret, a keyed journal and a legacy journal stay exactly as
    /// they were.
    #[cfg(unix)]
    #[test]
    fn an_unusable_journal_secret_refuses_the_store_and_is_never_replaced() {
        use std::os::unix::fs::PermissionsExt as _;

        const URI: &str = "file:///tmp/secret-unusable.md";
        let valid = |root: &Path| fs::read(root.join(JOURNAL_SECRET_FILE)).unwrap();
        let mut zeros = JOURNAL_SECRET_MAGIC.to_vec();
        zeros.extend_from_slice(&[0; JOURNAL_SECRET_LEN]);
        type Spoil = Box<dyn Fn(&Path, &[u8])>;
        let spoils: Vec<(&str, Spoil)> = vec![
            (
                "short",
                Box::new(|path: &Path, good: &[u8]| fs::write(path, &good[..20]).unwrap()),
            ),
            (
                "bad-magic",
                Box::new(|path: &Path, good: &[u8]| {
                    let mut bad = good.to_vec();
                    bad[0] ^= 0xff;
                    fs::write(path, bad).unwrap();
                }),
            ),
            (
                "zeros",
                Box::new(move |path: &Path, _: &[u8]| fs::write(path, &zeros).unwrap()),
            ),
            (
                "open-mode",
                Box::new(|path: &Path, _: &[u8]| {
                    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
                }),
            ),
            (
                "symlink",
                Box::new(|path: &Path, good: &[u8]| {
                    let target = path.with_file_name("elsewhere-key");
                    fs::write(&target, good).unwrap();
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
                    fs::remove_file(path).unwrap();
                    std::os::unix::fs::symlink(&target, path).unwrap();
                }),
            ),
            (
                "fifo",
                Box::new(|path: &Path, _: &[u8]| {
                    fs::remove_file(path).unwrap();
                    make_fifo(path);
                }),
            ),
        ];
        for (name, spoil) in spoils {
            let root = test_root(&format!("secret-{name}"));
            let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
            let mut store = crate::test_document_store();
            let keyed = journal_a_draft(&mut journals, &mut store, URI, "a keyed draft");
            drop(journals);
            let legacy = write_legacy_journal(&root, "file:///tmp/other.md", "x", "x y");
            let secret = root.join(JOURNAL_SECRET_FILE);
            spoil(&secret, &valid(&root));
            let before = |path: &Path| {
                (
                    fs::symlink_metadata(path).unwrap().file_type(),
                    fs::symlink_metadata(path).unwrap().permissions().mode(),
                    fs::read(&keyed).unwrap(),
                    fs::read(&legacy).unwrap(),
                )
            };
            let spoiled = before(&secret);
            let started = std::time::Instant::now();
            let error = match DocumentJournalStore::for_test(root.clone()) {
                Ok(_) => panic!("{name}: an unusable secret must refuse the store"),
                Err(error) => error,
            };
            // A hang detector, never a latency budget: a FIFO opened without
            // O_NONBLOCK parks forever.
            assert!(
                started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "{name}: the secret's read parked"
            );
            assert!(error.contains(JOURNAL_SECRET_FILE), "{name}: {error}");
            assert!(error.contains("never replaced"), "{name}: {error}");
            assert_eq!(before(&secret), spoiled, "{name}: nothing was touched");
            let _ = fs::remove_dir_all(root);
        }
    }

    /// A secret that is gone is minted afresh — a first run looks the same —
    /// and the journals keyed under the old one stay on disk untouched, read by
    /// no key again (never deleted, never mistaken for the new key's).
    #[test]
    fn a_deleted_journal_secret_is_reminted_and_its_journals_stay() {
        const URI: &str = "file:///tmp/secret-deleted.md";
        let root = test_root("secret-deleted");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let mut store = crate::test_document_store();
        let orphan = journal_a_draft(&mut journals, &mut store, URI, "an orphaned draft");
        let orphan_image = fs::read(&orphan).unwrap();
        let old_key = journals.key_for(URI);
        drop(journals);
        fs::remove_file(root.join(JOURNAL_SECRET_FILE)).unwrap();

        let journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        assert_ne!(journals.key_for(URI), old_key);
        let decision = journals.inspect_open(URI, b"").unwrap();
        assert_eq!(decision.recovered_text, None);
        assert_ne!(decision.path, orphan);
        assert_eq!(fs::read(&orphan).unwrap(), orphan_image);
        let _ = fs::remove_dir_all(root);
    }
}
