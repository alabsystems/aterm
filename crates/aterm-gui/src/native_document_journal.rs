// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable private crash journals for native documents, keyed by [`JournalDocumentKey`] under [`load_or_mint_journal_secret`] with one-time [`LegacyJournal`] migration.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_buffer::Seq;

use crate::document_store::{DocumentId, DocumentSnapshot};
use crate::durable::Flush;
use crate::native_document_host::{replace_file, sync_directory};
use crate::native_document_io::{
    ContentFingerprint, DraftJournalReducer, DurableCheckpoint, DurableSource, JOURNAL_FRAME_HEAD,
    JOURNAL_SECRET_LEN, JournalAppendPlan as RawJournalAppendPlan, JournalAppendProof,
    JournalAppendResult, JournalDocumentKey, JournalEdit, JournalError, JournalImageGeneration,
    JournalLanding, JournalOwnerId, JournalRecord, JournalReduction, JournalSecret, JournalStage,
    JournalTail, LegacyJournal, MAX_INSERT_BYTES, MAX_JOURNAL_BYTES, StepFault,
    StoredJournalRecovery, compact_journal, encode_journal_image, encode_journal_image_v1,
    frame_record, journal_is_format_1, recover_journal_for, recover_journal_image_for,
};

#[cfg(any(unix, windows))]
#[path = "native_document_journal_owner.rs"]
mod owner;

/// Where no image can carry an owner set (neither an extended attribute nor an alternate data stream), nothing is marked and nothing is taken: every journal stays, as it always did there.
#[cfg(not(any(unix, windows)))]
mod owner {
    use std::fs::File;
    use std::path::Path;

    use super::{ContentFingerprint, JournalDocumentKey, JournalImageGeneration, JournalOwnerId};

    #[derive(Debug)]
    pub(crate) enum JournalOwner {}

    impl JournalOwner {
        pub(crate) fn id(&self) -> JournalOwnerId {
            match *self {}
        }
        pub(super) fn run<T: Send + 'static>(
            &self,
            _work: impl FnOnce(&mut Custody) -> T + Send + 'static,
        ) -> Result<T, String> {
            match *self {}
        }
    }

    #[derive(Clone, Debug)]
    pub(crate) enum Registration {}
    pub(super) struct Custody;
    impl Custody {
        pub(super) fn revoke(&mut self, _paths: &[std::path::PathBuf]) {}
        pub(super) fn register(
            &mut self,
            _owner: &std::sync::Arc<JournalOwner>,
            _path: &Path,
            _key: JournalDocumentKey,
        ) -> Result<Registration, String> {
            Err("journal ownership unavailable".into())
        }
    }
    impl Registration {
        pub(super) fn live(&self) -> bool {
            match *self {}
        }
        pub(super) fn run<T: Send + 'static>(
            &self,
            _work: impl FnOnce(&mut Custody, u64) -> T + Send + 'static,
        ) -> Result<T, String> {
            match *self {}
        }
    }
    pub(super) fn operate<T>(
        path: &Path,
        _key: JournalDocumentKey,
        patience: super::JournalLockPatience,
        _custody: Option<(&mut Custody, u64)>,
        work: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        super::with_journal_lock(path, patience, work)
    }

    pub(crate) fn establish(_root: &Path) -> Result<JournalOwner, String> {
        Err("journal owners need extended attributes or streams".to_string())
    }

    pub(super) fn names_file(_path: &Path, _file: &File) -> bool {
        true
    }

    pub(super) fn uniquely_linked(_file: &File) -> std::io::Result<bool> {
        Ok(false)
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

    pub(super) fn mark_append(
        _path: &Path,
        _file: &File,
        _image: ContentFingerprint,
        _owner: Option<JournalOwnerId>,
    ) {
    }

    pub(super) fn adopt(_path: &Path, _image: ContentFingerprint, _owner: Option<JournalOwnerId>) {}
}

/// An append that would leave an image of more records than this compacts it instead.
const COMPACT_RECORDS: u64 = 128;
/// An append that would leave more than this of deltas after the image's last snapshot
/// ([`JournalTail::appended_since_snapshot`]) compacts it instead. The snapshots themselves
/// do not count: a draft larger than this still appends in place.
const COMPACT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryNotice {
    Recovered,
    /// The journal's whole records before a fault were recovered; the image is kept aside.
    Partial,
    DiskConflict,
    /// The journal did not decode.
    Corrupt,
}

impl RecoveryNotice {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Recovered => "Recovered an unsaved draft — the file on disk is unchanged",
            Self::Partial => {
                "Recovered an unsaved draft up to a part that could not be read; the file on disk is unchanged (the journal is kept)"
            }
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
    /// The keyed image the inspection read holds no draft ([`holds_no_draft`]).
    expected_clean: bool,
    /// The keyed image the inspection read is format 1 ([`journal_is_format_1`]), so
    /// [`DraftJournalHost::initialize`] republishes format 1: in an update from an aterm
    /// before format 2, that outgoing aterm reads the image back after this open (its Commit
    /// check of the carried draft, and on a rollback its re-seat), and it reads format 1
    /// alone. The first write after the open publishes format 2.
    keyed_format_1: bool,
    /// The legacy journal this open read in place of the keyed one.
    legacy: Option<LegacyJournal>,
}

#[derive(Clone, Debug)]
pub(crate) struct InitializedJournal {
    /// Shared one-shot transfer: cloned preparation results cannot adopt twice.
    ownership: Arc<std::sync::Mutex<ProvisionalOwnership>>,
    pub(crate) key: JournalDocumentKey,
    pub(crate) path: PathBuf,
    pub(crate) durable_seq: Seq,
    pub(crate) durable_text: Arc<str>,
    /// The image it published, as an appender holds it.
    pub(crate) tail: JournalTail,
    pub(crate) notice: Option<RecoveryNotice>,
    /// A legacy journal not yet retired.
    legacy: Option<LegacyJournal>,
}

#[derive(Debug)]
enum ProvisionalOwnership {
    Available(Option<owner::Registration>),
    Adopted,
}

impl InitializedJournal {
    /// Transfer provisional ownership once, in memory. Dropping an undelivered result
    /// releases its registration through the same asynchronous path as document close.
    fn take_registration(&self) -> Result<Option<owner::Registration>, String> {
        let mut ownership = self
            .ownership
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if matches!(&*ownership, ProvisionalOwnership::Available(Some(registration)) if !registration.live())
        {
            return Err("prepared journal registration was revoked".into());
        }
        match std::mem::replace(&mut *ownership, ProvisionalOwnership::Adopted) {
            ProvisionalOwnership::Available(registration) => Ok(registration),
            ProvisionalOwnership::Adopted => {
                Err("prepared journal ownership already adopted".into())
            }
        }
    }
}

/// The appender's content plan and the same per-open custody used at retirement.
#[derive(Clone, Debug)]
pub(crate) struct JournalAppendPlan {
    raw: RawJournalAppendPlan,
    registration: Option<owner::Registration>,
}

impl std::ops::Deref for JournalAppendPlan {
    type Target = RawJournalAppendPlan;
    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}

pub(crate) trait AppendWork {
    fn raw(&self) -> &RawJournalAppendPlan;
    fn registration(&self) -> Option<&owner::Registration> {
        None
    }
}
impl AppendWork for RawJournalAppendPlan {
    fn raw(&self) -> &RawJournalAppendPlan {
        self
    }
}
impl AppendWork for JournalAppendPlan {
    fn raw(&self) -> &RawJournalAppendPlan {
        &self.raw
    }
    fn registration(&self) -> Option<&owner::Registration> {
        self.registration.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct JournalRewriteGeneration(pub(crate) u64);

#[derive(Clone, Debug)]
pub(crate) struct JournalRewritePlan {
    registration: Option<owner::Registration>,
    pub(crate) generation: JournalRewriteGeneration,
    pub(crate) key: JournalDocumentKey,
    pub(crate) base_durable: Seq,
    pub(crate) expected_image: ContentFingerprint,
    pub(crate) target_seq: Seq,
    pub(crate) target_text: Arc<str>,
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) fingerprint: ContentFingerprint,
    /// [`Self::bytes`]' tail, which appends after the rewrite extend.
    pub(crate) tail: JournalTail,
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
    /// This store's owner identity, named on every image it publishes.
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
    registration: Option<owner::Registration>,
    key: JournalDocumentKey,
    path: PathBuf,
    reducer: DraftJournalReducer,
    durable_text: Arc<str>,
    /// The image this store last published or appended to, held beside the journal: an append
    /// proves the file is still it from its length and last frame ([`JournalTail`]).
    image: JournalTail,
    desired: DocumentSnapshot,
    append_text: Option<Arc<str>>,
    next_rewrite_generation: u64,
    rewrite_inflight: Option<InflightRewrite>,
    pending_checkpoint: Option<SavedBaseline>,
    /// The image's first snapshot, as a fingerprint and a sequence.
    baseline: ContentFingerprint,
    baseline_seq: Seq,
    /// Whether `baseline` is the file's contents.
    baseline_is_file: bool,
    /// The image on disk may no longer be this store's own.
    reseat_owed: bool,
    reseat_inflight: Option<JournalReseatPlan>,
    /// The last re-seat failed.
    reseat_failed: bool,
    /// The last append failed and nothing has made the head durable.
    append_failed: bool,
    /// A legacy journal this document was migrated from and that is not yet retired.
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

/// Re-establish this process's journal after an update's successor wrote over it.
#[derive(Clone, Debug)]
pub(crate) struct JournalReseatPlan {
    registration: Option<owner::Registration>,
    pub(crate) generation: JournalRewriteGeneration,
    pub(crate) document: DocumentId,
    pub(crate) key: JournalDocumentKey,
    /// The image this store last published.
    pub(crate) ours: ContentFingerprint,
    pub(crate) baseline: ContentFingerprint,
    pub(crate) baseline_seq: Seq,
    /// [`JournalEntry::baseline_is_file`].
    pub(crate) baseline_is_file: bool,
    /// What this store last proved durable, and what it holds now.
    pub(crate) durable_text: Arc<str>,
    pub(crate) target_seq: Seq,
    pub(crate) target_text: Arc<str>,
    /// Retired before this re-seat publishes ([`LegacyJournal`]).
    pub(crate) retire_legacy: Option<LegacyJournal>,
    /// Named on the image it publishes, or joined to the one it finds.
    pub(crate) owner: Option<JournalOwnerId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalReseatResult {
    /// The image on disk is still the one this store published.
    Unchanged,
    /// A fresh image recovering to the plan's target was published.
    Republished {
        image: JournalTail,
        baseline: ContentFingerprint,
        baseline_seq: Seq,
        baseline_is_file: bool,
        preserved: bool,
    },
    Failed(String),
}

/// Why a document's unsaved draft cannot be carried through an update right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftCarryRefusal {
    /// The document has no journal in this process.
    Unjournaled,
    /// An append, checkpoint or re-seat is on the worker now.
    Writing,
    /// A re-seat is owed and has not run.
    Reseating,
    /// The last re-seat failed (it is retried, but not in silence).
    ReseatFailed,
    /// The last append failed and no write has landed since (it is retried, but not in silence).
    AppendFailed,
    /// The latest edit is not durable, nothing is writing it and no write of it failed.
    Behind,
}

/// What this store vouches for about one document's head.
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

/// UI-thread owner for latest-wins serialization.
pub(crate) struct DocumentJournalStore {
    host: DraftJournalHost,
    entries: BTreeMap<DocumentId, JournalEntry>,
    /// Open documents that keep no journal ([`Self::decline`]): every write to theirs is a no-op.
    declined: BTreeSet<DocumentId>,
    /// See [`Self::set_reseat_fenced`].
    reseat_fenced: bool,
}

/// Whether `canonical_uri` names a private edit's document ([`aterm_types::private_edit`]): one
/// amacs keeps no draft journal and no closed-buffer recovery of, and serves no `doc read` of.
pub(crate) fn private_uri(canonical_uri: &str) -> bool {
    crate::native_document_host::file_uri_path(canonical_uri)
        .is_ok_and(|path| aterm_types::private_edit::is_private(&path))
}

impl DocumentJournalStore {
    pub(crate) fn system_default() -> Result<Self, String> {
        Ok(Self {
            host: DraftJournalHost::system_default()?,
            entries: BTreeMap::new(),
            declined: BTreeSet::new(),
            reseat_fenced: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(root: PathBuf) -> Result<Self, String> {
        Ok(Self {
            host: DraftJournalHost::new(root)?,
            entries: BTreeMap::new(),
            declined: BTreeSet::new(),
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
                registration: initialized.take_registration()?,
                key: initialized.key,
                path: initialized.path.clone(),
                reducer: DraftJournalReducer::new_with_key(
                    current.id,
                    initialized.key,
                    initialized.durable_seq,
                ),
                durable_text: initialized.durable_text.clone(),
                image: initialized.tail,
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

    /// Owe a re-seat on every journal this store writes (see [`JournalEntry::reseat_owed`]).
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

    /// Whether any journal write (append, checkpoint, or re-seat) is on the worker now.
    pub(crate) fn writes_in_flight(&self) -> bool {
        self.entries.values().any(entry_busy)
    }

    /// The documents whose journal has a write on the worker now.
    pub(crate) fn busy_documents(&self) -> impl Iterator<Item = DocumentId> + '_ {
        self.entries
            .iter()
            .filter(|(_, entry)| entry_busy(entry))
            .map(|(document, _)| *document)
    }

    pub(crate) fn reseat_owed(&self, document: DocumentId) -> bool {
        self.entries
            .get(&document)
            .is_some_and(|entry| entry.reseat_owed || entry.reseat_inflight.is_some())
    }

    /// Whether `document`'s last append or re-seat failed and nothing has landed.
    pub(crate) fn write_failed(&self, document: DocumentId) -> bool {
        self.entries.get(&document).is_some_and(|entry| {
            (entry.append_failed && entry.desired.seq > entry.reducer.durable_seq())
                || (entry.reseat_failed && (entry.reseat_owed || entry.reseat_inflight.is_some()))
        })
    }

    /// Documents whose journal owes work nothing is doing.
    pub(crate) fn owed_documents(&self) -> Vec<DocumentId> {
        self.entries
            .iter()
            .filter(|(_, entry)| {
                !entry_busy(entry)
                    && (entry.reseat_owed
                        || entry.pending_checkpoint.is_some()
                        || entry.desired.seq > entry.reducer.durable_seq())
            })
            .map(|(document, _)| *document)
            .collect()
    }

    /// The memory half of carrying `head` through an update.
    pub(crate) fn carry_candidate(
        &self,
        head: &DocumentSnapshot,
    ) -> Result<DraftCarryCandidate, DraftCarryRefusal> {
        let entry = self
            .entries
            .get(&head.id)
            .ok_or(DraftCarryRefusal::Unjournaled)?;
        if entry.reseat_failed && (entry.reseat_owed || entry.reseat_inflight.is_some()) {
            return Err(DraftCarryRefusal::ReseatFailed);
        }
        if entry.append_failed && entry.desired.seq > entry.reducer.durable_seq() {
            return Err(DraftCarryRefusal::AppendFailed);
        }
        if entry_busy(entry) {
            return Err(DraftCarryRefusal::Writing);
        }
        if entry.reseat_owed {
            return Err(DraftCarryRefusal::Reseating);
        }
        if entry.pending_checkpoint.is_some() {
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

    /// Keep no journal for `document` (a private edit's, [`private_uri`]): remove every journal an
    /// earlier open of `canonical_uri` left, keyed or legacy, with its preserved copies, lock and
    /// write temporaries, and from now on take its commits and checkpoints as no-ops and owe it
    /// no effect. An error leaves the document declined, and names what could not be removed.
    pub(crate) fn decline(
        &mut self,
        document: DocumentId,
        canonical_uri: &str,
    ) -> Result<(), String> {
        self.entries.remove(&document);
        self.declined.insert(document);
        self.host.purge(canonical_uri)
    }

    /// Whether `document` keeps no journal ([`Self::decline`]).
    pub(crate) fn declined(&self, document: DocumentId) -> bool {
        self.declined.contains(&document)
    }

    pub(crate) fn observe_commit(&mut self, snapshot: &DocumentSnapshot) -> Result<(), String> {
        if self.declined(snapshot.id) {
            return Ok(());
        }
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
        if self.declined(checkpoint.document) {
            return Ok(());
        }
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
        if self.declined(document) {
            return Ok(None);
        }
        let reseat_fenced = self.reseat_fenced;
        let Some(entry) = self.entries.get_mut(&document) else {
            return Err("document journal was not initialized".to_string());
        };
        if entry_busy(entry) {
            return Ok(None);
        }
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
                registration: entry.registration.clone(),
                generation,
                document,
                key: entry.key,
                ours: entry.image.fingerprint(),
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
                entry.image.fingerprint(),
                &disk,
                &entry.desired,
            )
            .map_err(|error| format!("could not encode journal checkpoint: {error:?}"))?;
            plan.retire_legacy = entry.legacy.clone();
            plan.owner = self.host.owner_id();
            plan.registration = entry.registration.clone();
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
        // One edit from the durable text to the head, however many commits coalesced behind the
        // append in flight: an append writes about what changed, never the whole document.
        let edit = single_edit(&entry.durable_text, &entry.desired.text);
        let mut plan = if edit.insert.len() <= MAX_INSERT_BYTES {
            entry
                .reducer
                .plan_delta(entry.reducer.durable_seq(), entry.desired.seq, vec![edit])
                .map_err(|error| format!("could not plan journal delta: {error:?}"))?
        } else {
            entry
                .reducer
                .plan_snapshot(&entry.desired)
                .map_err(|error| format!("could not plan journal snapshot: {error:?}"))?
        };
        plan.expected_tail = Some(entry.image);
        plan.retire_legacy = entry.legacy.clone();
        plan.owner = self.host.owner_id();
        entry.append_text = Some(entry.desired.text.clone());
        Ok(Some(JournalEffect::Append {
            path: entry.path.clone(),
            key: entry.key,
            plan: JournalAppendPlan {
                raw: plan,
                registration: entry.registration.clone(),
            },
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
        let published = match &result {
            JournalAppendResult::Committed(proof) => Some(proof.published),
            _ => None,
        };
        match entry.reducer.complete(generation, result) {
            JournalReduction::Durable(checkpoint) => {
                if let Some(text) = entry.append_text.take() {
                    entry.durable_text = text;
                }
                if let Some(published) = published {
                    entry.image = published;
                }
                entry.append_failed = false;
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

    /// Restore a checkpoint rewrite that was planned but never admitted to the worker.
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
                entry.image = plan.tail;
                entry.append_text = None;
                entry.append_failed = false;
                entry.legacy = None;
                entry.baseline = ContentFingerprint::of(checkpoint.text.as_bytes());
                entry.baseline_seq = checkpoint.seq;
                entry.baseline_is_file = true;
                JournalCompletion::Durable {
                    document,
                    seq: plan.target_seq,
                }
            }
            result => {
                // A failed or unproved checkpoint still owns its retirement work. Keep
                // the newest intent, including one queued while this write was inflight.
                if entry
                    .pending_checkpoint
                    .as_ref()
                    .is_none_or(|pending| checkpoint.seq > pending.seq)
                {
                    entry.pending_checkpoint = Some(checkpoint);
                }
                let message = match result {
                    JournalRewriteResult::Committed(_) => {
                        "journal checkpoint proof did not match the pending image".to_string()
                    }
                    JournalRewriteResult::Failed(message) => {
                        format!("journal checkpoint failed: {message}")
                    }
                };
                JournalCompletion::Failed { document, message }
            }
        }
    }

    /// Apply the outcome of a completed journal re-seat.
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
                entry.image = image;
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

    /// Fence or unfence journal re-seats while a handoff attempt is in progress.
    pub(crate) const fn set_reseat_fenced(&mut self, fenced: bool) {
        self.reseat_fenced = fenced;
    }

    /// Whether `document`'s journal has nothing left to do: no write on the worker, no re-seat owed, no checkpoint queued, no legacy journal left to retire, and its head durable.
    pub(crate) fn settled(&self, document: DocumentId) -> bool {
        self.entries.get(&document).is_none_or(|entry| {
            !entry_busy(entry)
                && !entry.reseat_owed
                && entry.pending_checkpoint.is_none()
                && entry.legacy.is_none()
                && entry.desired.seq == entry.reducer.durable_seq()
        })
    }

    /// Retire only entries whose durable work has settled. A canceled caller cannot
    /// discard a reducer, dirty head or inflight write merely by losing its last view.
    pub(crate) fn forget(&mut self, documents: &[DocumentId]) -> bool {
        if documents.iter().any(|document| !self.settled(*document)) {
            return false;
        }
        for document in documents {
            self.declined.remove(document);
            self.entries.remove(document);
        }
        true
    }

    /// Wait for previously enqueued custody work in tests; execution still happens on
    /// the production worker, never inline on the test's calling thread.
    #[cfg(test)]
    pub(crate) fn await_worker_for_test(&self) {
        if let Some(owner) = &self.host.owner {
            owner.run(|_| ()).expect("journal custody worker barrier");
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
        self.entries
            .get(&document)
            .map(|entry| entry.image.fingerprint())
    }
}

/// Hold the journal's advisory lock while the returned handle lives.
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
    static FORKED_LOCK_COPIES: std::cell::RefCell<Option<Arc<std::sync::Mutex<Vec<File>>>>> =
        const { std::cell::RefCell::new(None) };
}

/// While this lives, every journal lock this thread takes leaves behind a second descriptor.
#[cfg(all(test, unix))]
pub(crate) struct ForkedJournalLockCopies(());

#[cfg(all(test, unix))]
impl ForkedJournalLockCopies {
    pub(crate) fn start() -> Self {
        FORKED_LOCK_COPIES.with(|copies| {
            *copies.borrow_mut() = Some(Arc::new(std::sync::Mutex::new(Vec::new())))
        });
        Self(())
    }

    /// How many held locks have been copied so far.
    pub(crate) fn copied(&self) -> usize {
        FORKED_LOCK_COPIES.with(|copies| {
            copies
                .borrow()
                .as_ref()
                .map_or(0, |copies| copies.lock().unwrap().len())
        })
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
    static BETWEEN_LOCK_OPEN_AND_TAKE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `then` once, on this thread, between the next journal lock's open and its take.
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

/// The fork a [`ForkedJournalLockCopies`] stands in for, at the instant the lock is held.
#[cfg(all(test, unix))]
fn copy_held_lock_for_test(lock: &File) {
    FORKED_LOCK_COPIES.with(|copies| {
        if let Some(copies) = copies.borrow_mut().as_mut() {
            copies
                .lock()
                .unwrap()
                .push(lock.try_clone().expect("copy the held journal lock"));
        }
    });
}

const fn entry_busy(entry: &JournalEntry) -> bool {
    entry.append_text.is_some()
        || entry.rewrite_inflight.is_some()
        || entry.reseat_inflight.is_some()
}

/// The disk half of carrying a draft through an update.
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

const fn holds_no_draft(recovered: &StoredJournalRecovery) -> bool {
    recovered.record_count == 1
}

struct OpenReading {
    /// Its draft was made against exactly the file on disk and is not the file.
    recover: bool,
    /// Its draft was made against other contents and is not the file.
    keep_aside: bool,
    /// It holds a draft made against exactly the file on disk, equal to it or not — the draft an outgoing older aterm's Commit check reads back ([`LegacyJournal`]).
    made_against_disk: bool,
}

fn read_for_open(recovered: &StoredJournalRecovery, disk_bytes: &[u8]) -> OpenReading {
    // Opened or saved and never edited since: nothing to recover or keep aside, whatever the disk holds now; `initialize` replaces the image with the file as it is.
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

/// What a legacy draft that wins the open says of the keyed image it sets aside: a keyed draft
/// other than the legacy one is [`RecoveryNotice::DiskConflict`]; a keyed image that did not
/// decode, or one with a fault after its whole records ([`report_fault`]'s case) whose prefix
/// holds no other draft, is [`RecoveryNotice::Corrupt`], so a damaged keyed image is never set
/// aside unsaid.
fn keyed_set_aside_by_legacy(
    keyed: &Result<StoredJournalRecovery, crate::native_document_io::JournalError>,
    keyed_fault: Option<&StepFault>,
    legacy: &StoredJournalRecovery,
) -> Option<RecoveryNotice> {
    match keyed {
        Err(_) => Some(RecoveryNotice::Corrupt),
        Ok(keyed) if !holds_no_draft(keyed) && keyed.text != legacy.text => {
            Some(RecoveryNotice::DiskConflict)
        }
        Ok(_) if keyed_fault.is_some() => Some(RecoveryNotice::Corrupt),
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

/// An open read the whole records before `fault` (a cut astream reads as a fault, such as one
/// past a frame nested in a record's text, or damage): it says so. A recovered draft is
/// [`RecoveryNotice::Partial`]; one kept aside for disk content still says that; anything else
/// recovered nothing and is [`RecoveryNotice::Corrupt`]. The caller keeps the image aside.
fn report_fault(decision: &mut JournalOpenDecision, fault: &StepFault) {
    aterm_log::warn!(
        "recovery journal has a fault after its whole records ({fault:?}); they were read and \
         the journal is kept aside"
    );
    decision.notice = Some(match decision.notice {
        Some(RecoveryNotice::Recovered) => RecoveryNotice::Partial,
        Some(RecoveryNotice::DiskConflict) => RecoveryNotice::DiskConflict,
        _ => RecoveryNotice::Corrupt,
    });
}

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
    if let Some(registration) = &plan.registration {
        let mut plan = plan.clone();
        plan.registration = None;
        let path = path.to_path_buf();
        return registration
            .run(move |custody, id| {
                reseat_owned(
                    &path,
                    &plan,
                    JournalLockPatience::Worker,
                    Some((custody, id)),
                )
            })
            .unwrap_or_else(JournalReseatResult::Failed);
    }
    reseat_owned(path, plan, patience, None)
}

fn reseat_owned(
    path: &Path,
    plan: &JournalReseatPlan,
    patience: JournalLockPatience,
    custody: Option<(&mut owner::Custody, u64)>,
) -> JournalReseatResult {
    if let Err(message) = retire_owed_legacy(plan.retire_legacy.as_ref(), patience) {
        return JournalReseatResult::Failed(message);
    }
    let result = owner::operate(path, plan.key, patience, custody, || {
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
        let recovered = existing.as_deref().and_then(|bytes| {
            recover_journal_image_for(plan.key, bytes)
                .ok()
                .map(|(read, tail, fault)| {
                    (
                        read,
                        fault.is_none()
                            && tail.is_none_or(|tail| tail.byte_len() == bytes.len() as u64),
                    )
                })
        });
        let holds_nothing_new = recovered.as_ref().is_some_and(|(recovered, complete)| {
            *complete
                && (recovered.text == *plan.durable_text
                    || recovered.text == *plan.target_text
                    || holds_no_draft(recovered))
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
        let (bytes, image) =
            encode_journal_image(&records).map_err(|error| format!("encode: {error:?}"))?;
        atomic_replace_locked(path, &bytes, found, plan.owner)
            .map_err(|error| error.to_string())?;
        Ok(Some(JournalReseatResult::Republished {
            image,
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

const fn synthetic_snapshot(document: DocumentId, seq: Seq, text: Arc<str>) -> DocumentSnapshot {
    DocumentSnapshot {
        id: document,
        seq,
        text,
    }
}

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
    let before_tail = &before[prefix..];
    let after_tail = &after[prefix..];
    let mut suffix = before_tail
        .as_bytes()
        .iter()
        .rev()
        .zip(after_tail.as_bytes().iter().rev())
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| before_tail.len().min(after_tail.len()));
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
            #[cfg(any(unix, windows))]
            owner::spawn_launch_sweep(host.root.clone());
            Ok(host)
        }
    }

    /// The host over `root`, with the journal secret read from it (or minted into it).
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
        if let Some(owner) = &self.owner {
            let host = self.clone();
            let uri = canonical_uri.to_string();
            let bytes = disk_bytes.to_vec();
            return owner.run(move |_| host.inspect_owned(&uri, &bytes))?;
        }
        self.inspect_owned(canonical_uri, disk_bytes)
    }

    fn inspect_owned(
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
            keyed_format_1: false,
            legacy: None,
        };
        let keyed = read_journal_image(&path)
            .map_err(|error| format!("could not read recovery journal: {error}"))?;
        decision.keyed_format_1 = keyed.as_deref().is_some_and(journal_is_format_1);
        let mut keyed_fault = None;
        let keyed_recovery = keyed.as_deref().map(|bytes| {
            decision.expected_image = JournalImageGeneration::of(bytes);
            recover_journal_image_for(key, bytes).map(|(recovered, tail, fault)| {
                decision.preserve_existing |= fault.is_some()
                    || tail.is_some_and(|tail| tail.byte_len() < bytes.len() as u64);
                keyed_fault = fault;
                recovered
            })
        });
        decision.expected_clean = !decision.preserve_existing
            && keyed_recovery
                .as_ref()
                .is_some_and(|recovered| recovered.as_ref().is_ok_and(holds_no_draft));
        let legacy_key = JournalDocumentKey::legacy_unkeyed(canonical_uri);
        let legacy_path = self.legacy_path_for(legacy_key);
        let legacy = read_journal_image(&legacy_path)
            .map_err(|error| format!("could not read legacy recovery journal: {error}"))?;
        if let Some(legacy_bytes) = legacy {
            let image = JournalImageGeneration::of(&legacy_bytes);
            match recover_journal_image_for(legacy_key, &legacy_bytes) {
                Ok((recovered, tail, fault)) => {
                    let torn = fault.is_some()
                        || tail.is_some_and(|tail| tail.byte_len() < legacy_bytes.len() as u64);
                    let reading = read_for_open(&recovered, disk_bytes);
                    if reading.made_against_disk || reading.keep_aside {
                        decision.legacy = Some(LegacyJournal {
                            path: legacy_path,
                            image,
                            carried: reading.made_against_disk,
                            keep_aside: reading.keep_aside || torn,
                        });
                        let set_aside = keyed_recovery.as_ref().and_then(|keyed| {
                            keyed_set_aside_by_legacy(keyed, keyed_fault.as_ref(), &recovered)
                        });
                        if let Some(fault) = &keyed_fault {
                            aterm_log::warn!(
                                "recovery journal has a fault after its whole records \
                                 ({fault:?}); a legacy draft wins the open and it is kept aside"
                            );
                        }
                        reading.apply(&mut decision, recovered);
                        if let Some(fault) = fault {
                            report_fault(&mut decision, &fault);
                        }
                        if let Some(notice) = set_aside {
                            aterm_log::warn!(
                                "a legacy recovery journal was written after this document's \
                                 keyed one; the keyed draft is kept aside"
                            );
                            decision.preserve_existing = true;
                            decision.notice.get_or_insert(notice);
                        }
                        return Ok(decision);
                    }
                    decision.legacy = Some(LegacyJournal {
                        path: legacy_path,
                        image,
                        carried: false,
                        keep_aside: torn,
                    });
                }
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
                decision.preserve_existing |= reading.keep_aside;
                reading.apply(&mut decision, recovered);
                if let Some(fault) = keyed_fault {
                    report_fault(&mut decision, &fault);
                }
            }
            Err(error) => {
                aterm_log::warn!("recovery journal did not decode; kept aside: {error:?}");
                decision.notice = Some(RecoveryNotice::Corrupt);
                decision.preserve_existing = true;
            }
        }
        Ok(decision)
    }

    /// Publish a fresh, sequence-aligned image before any document view becomes visible.
    pub(crate) fn initialize(
        &self,
        decision: JournalOpenDecision,
        disk: &DocumentSnapshot,
        current: &DocumentSnapshot,
    ) -> Result<InitializedJournal, String> {
        if let Some(owner) = &self.owner {
            let host = self.clone();
            let disk = disk.clone();
            let current = current.clone();
            return owner.run(move |custody| {
                host.initialize_owned(decision, &disk, &current, Some(custody))
            })?;
        }
        self.initialize_owned(decision, disk, current, None)
    }

    fn initialize_owned(
        &self,
        decision: JournalOpenDecision,
        disk: &DocumentSnapshot,
        current: &DocumentSnapshot,
        custody: Option<&mut owner::Custody>,
    ) -> Result<InitializedJournal, String> {
        if disk.id != current.id {
            return Err("journal initialization mixed document identities".to_string());
        }
        let mut records = vec![JournalRecord::snapshot_for(decision.key, disk)];
        if current.seq != disk.seq || current.text != disk.text {
            records.push(JournalRecord::snapshot_for(decision.key, current));
        }
        // A format-1 image the inspection read (an aterm before format 2 wrote it) is
        // republished in format 1 (`JournalOpenDecision::keyed_format_1`): its draft, read
        // through the format-1 decoder, is in `current`, and the first write after this open
        // compacts it into format 2. One that did not decode was set aside untouched first.
        let encoded = if decision.keyed_format_1 {
            encode_journal_image_v1(&records)
        } else {
            encode_journal_image(&records)
        };
        let (bytes, tail) = encoded.map_err(|error| format!("{error:?}"))?;
        let registration = with_journal_lock(
            &decision.path,
            JournalLockPatience::EventLoop,
            || {
                let current = observe_journal_image(&decision.path)?;
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
                    .map_err(|error| error.to_string())?;
                match (custody, &self.owner) {
                    (Some(custody), Some(owner)) => custody
                        .register(owner, &decision.path, decision.key)
                        .map(Some),
                    _ => Ok(None),
                }
            },
        )?;
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
            ownership: Arc::new(std::sync::Mutex::new(ProvisionalOwnership::Available(
                registration,
            ))),
            key: decision.key,
            path: decision.path,
            durable_seq: current.seq,
            durable_text: current.text.clone(),
            tail,
            notice: decision.notice,
            legacy,
        })
    }

    /// A keyed journal's name.
    pub(crate) fn path_for(&self, key: JournalDocumentKey) -> PathBuf {
        self.root.join(format!("{:016x}.mac.atdj", key.0))
    }

    fn legacy_path_for(&self, legacy: JournalDocumentKey) -> PathBuf {
        self.root.join(format!("{:016x}.atdj", legacy.0))
    }

    /// Remove every file of `canonical_uri`'s journals from the root: the keyed and the legacy
    /// journal, each one's preserved copies (`<stem>.preserved-<n>.atdj`), lock (`.<name>.lock`)
    /// and write temporaries (`.<name>.<pid>-<nonce>.tmp`), then sync the root. A file already
    /// gone is no error. Registered cleanup is an acknowledged worker operation,
    /// ordered with releases and writes; it is not the UI retirement/drop path.
    fn purge(&self, canonical_uri: &str) -> Result<(), String> {
        if let Some(owner) = &self.owner {
            let host = self.clone();
            let uri = canonical_uri.to_owned();
            return owner.run(move |custody| {
                let paths = host.journal_paths(&uri);
                custody.revoke(&paths);
                host.purge_paths(paths)
            })?;
        }
        self.purge_paths(self.journal_paths(canonical_uri))
    }

    fn journal_paths(&self, canonical_uri: &str) -> [PathBuf; 2] {
        [
            self.path_for(self.key_for(canonical_uri)),
            self.legacy_path_for(JournalDocumentKey::legacy_unkeyed(canonical_uri)),
        ]
    }

    fn purge_paths(&self, journals: [PathBuf; 2]) -> Result<(), String> {
        #[cfg(all(test, any(unix, windows)))]
        owner::check_fs_thread();
        let mut doomed = Vec::new();
        for journal in &journals {
            let (Some(name), Some(stem)) = (
                journal.file_name().and_then(|name| name.to_str()),
                journal.file_stem().and_then(|stem| stem.to_str()),
            ) else {
                continue;
            };
            doomed.push(journal.clone());
            doomed.push(self.root.join(format!(".{name}.lock")));
            let preserved = format!("{stem}.preserved-");
            let temporary = format!(".{name}.");
            let listing = match fs::read_dir(&self.root) {
                Ok(listing) => listing,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(format!("could not list the draft journals: {error}")),
            };
            for entry in listing.flatten() {
                let file = entry.file_name();
                let Some(file) = file.to_str() else {
                    continue;
                };
                let copy = file
                    .strip_prefix(&preserved)
                    .and_then(|rest| rest.strip_suffix(".atdj"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
                let partial = file
                    .strip_prefix(&temporary)
                    .is_some_and(|rest| rest.ends_with(".tmp"));
                if copy || partial {
                    doomed.push(entry.path());
                }
            }
        }
        let mut removed = false;
        for path in doomed {
            match fs::remove_file(&path) {
                Ok(()) => removed = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "could not remove the draft journal {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        if removed {
            sync_directory(&self.root, Flush::Durable)
                .map_err(|error| format!("could not sync the draft journals: {error}"))?;
        }
        Ok(())
    }

    fn preserve_locked(&self, path: &Path, expected: JournalImageGeneration) -> Result<(), String> {
        debug_assert_eq!(path.parent(), Some(self.root.as_path()));
        preserve_journal_locked(path, expected)
    }
}

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
        return sync_directory(root, Flush::Ordered)
            .map_err(|error| format!("could not sync preserved recovery journal: {error}"));
    }
    Err("too many preserved recovery journals for this document".to_string())
}

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
        sync_directory(root, Flush::Ordered)
            .map_err(|error| format!("could not sync retired legacy recovery journal: {error}"))
    })
}

fn retire_owed_legacy(
    legacy: Option<&LegacyJournal>,
    patience: JournalLockPatience,
) -> Result<(), String> {
    legacy.map_or(Ok(()), |legacy| {
        retire_legacy_journal(legacy, patience).map_err(|error| format!("legacy journal: {error}"))
    })
}

const JOURNAL_SECRET_FILE: &str = ".journal-key";
const JOURNAL_SECRET_MAGIC: [u8; 8] = *b"ATDJKEY1";
const JOURNAL_SECRET_FILE_LEN: usize = JOURNAL_SECRET_MAGIC.len() + JOURNAL_SECRET_LEN;

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
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Open the secret without following or waiting on a hostile final component; the handle, not the pathname, is then proved a regular file.
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

/// Registered appends execute on their lease's custody worker. For an unregistered
/// content plan, `patience` declares the thread executing the disk primitive.
pub(crate) fn execute_journal_append(
    path: &Path,
    key: JournalDocumentKey,
    plan: &impl AppendWork,
    patience: JournalLockPatience,
) -> JournalAppendResult {
    if let Some(registration) = plan.registration() {
        let plan = plan.raw().clone();
        let path = path.to_path_buf();
        return registration
            .run(move |custody, id| {
                append_owned(
                    &path,
                    key,
                    &plan,
                    JournalLockPatience::Worker,
                    Some((custody, id)),
                )
            })
            .unwrap_or_else(|message| JournalAppendResult::Failed {
                stage: JournalStage::Append,
                message,
            });
    }
    append_owned(path, key, plan.raw(), patience, None)
}

fn append_owned(
    path: &Path,
    key: JournalDocumentKey,
    plan: &RawJournalAppendPlan,
    patience: JournalLockPatience,
    custody: Option<(&mut owner::Custody, u64)>,
) -> JournalAppendResult {
    if let Err(message) = retire_owed_legacy(plan.retire_legacy.as_ref(), patience) {
        return JournalAppendResult::Failed {
            stage: JournalStage::Append,
            message,
        };
    }
    let result = owner::operate(path, key, patience, custody, || {
        let tail = plan
            .expected_tail
            .ok_or_else(|| "preflight: journal plan has no bound disk image".to_string())?;
        // A format-1 image an open republished has no frame to follow: its first append
        // compacts it into format 2.
        if !tail.appends_in_place() {
            return compact_appending(path, key, plan, tail);
        }
        let (frames, after) =
            frame_record(&tail, &plan.bytes).map_err(|error| format!("encode: {error:?}"))?;
        if after.records() > COMPACT_RECORDS
            || after.appended_since_snapshot() > COMPACT_BYTES
            || after.byte_len() > MAX_JOURNAL_BYTES as u64
        {
            return compact_appending(path, key, plan, tail);
        }
        let file = open_journal_image(path).map_err(|error| format!("preflight: {error}"))?;
        holds_tail(&file, &tail)?;
        // Preservation can hard-link the current image before a replacement succeeds. An
        // append must not change that saved copy; replace the named image instead. The lock
        // serializes this check with cooperating preservation and publication writers.
        if !owner::uniquely_linked(&file).map_err(|error| format!("preflight: {error}"))? {
            return compact_appending(path, key, plan, tail);
        }
        // In place after the last whole record, then durable before acknowledging the new
        // checkpoint. An ordering barrier alone would allow power loss to discard a record
        // after its durable sequence had advanced. An interrupted append leaves the whole
        // records before it readable: astream reads most cuts as a torn tail, dropped, and a
        // cut past a frame nested in the record's text as a fault, which an open reads past
        // to those records while it keeps the image aside (`DraftJournalHost::inspect_open`).
        let landed = write_at(&file, &frames, tail.byte_len())
            .and_then(|()| crate::durable::finish(&file, Flush::Durable));
        if let Err(error) = landed {
            // Nobody was told of this record: take back what of it reached the file, so the
            // next append still finds the image this store holds.
            let _ = file.set_len(tail.byte_len());
            return Err(format!("append: {error}"));
        }
        owner::mark_append(path, &file, after.fingerprint(), plan.owner);
        Ok(JournalAppendProof {
            record_len: plan.bytes.len(),
            encoded_fingerprint: plan.encoded_fingerprint,
            published: after,
            file_synced: true,
            landing: JournalLanding::Appended,
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

/// Compare the expected append boundary under the journal lock: the length and last frame's
/// head (its CRC, step and prefix fingerprint, [`JournalTail`]). Protocol-following writers
/// change this boundary when appending or replacing an image. This does not rescan earlier
/// payloads or detect arbitrary in-place corruption; the full decoder checks their frames.
fn holds_tail(file: &File, tail: &JournalTail) -> Result<(), String> {
    let len = file
        .metadata()
        .map_err(|error| format!("preflight: {error}"))?
        .len();
    if len != tail.byte_len() {
        return Err(format!(
            "preflight: journal image changed (expected {} bytes, found {len})",
            tail.byte_len()
        ));
    }
    let (at, head) = tail.last_frame();
    let mut found = [0_u8; JOURNAL_FRAME_HEAD];
    read_at(file, &mut found, at).map_err(|error| format!("preflight: {error}"))?;
    if found != *head {
        return Err(format!(
            "preflight: journal image changed (its last frame, at byte {at}, is not the one \
             this store wrote)"
        ));
    }
    Ok(())
}

fn read_at(mut file: &File, buf: &mut [u8], at: u64) -> std::io::Result<()> {
    use std::io::{Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(buf)
}

fn write_at(mut file: &File, bytes: &[u8], at: u64) -> std::io::Result<()> {
    use std::io::{Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(at))?;
    file.write_all(bytes)
}

/// An append that takes the image past a compaction bound ([`COMPACT_RECORDS`],
/// [`COMPACT_BYTES`], [`MAX_JOURNAL_BYTES`]), or the first after an open republished a
/// format-1 image ([`JournalTail::appends_in_place`]): the image read whole and proved to be
/// `tail`'s, the plan's record after it, compacted ([`compact_journal`]) into format 2 and
/// renamed over the journal with it and its folder durable, as a checkpoint is.
fn compact_appending(
    path: &Path,
    key: JournalDocumentKey,
    plan: &RawJournalAppendPlan,
    tail: JournalTail,
) -> Result<JournalAppendProof, String> {
    let existing = read_existing_journal(path, "preflight")?;
    let actual = JournalImageGeneration::of(&existing);
    if actual.fingerprint != tail.fingerprint() {
        return Err(format!(
            "preflight: journal image changed (expected {:?}, found {:?})",
            tail.fingerprint(),
            actual.fingerprint
        ));
    }
    let (image, published) = compact_journal(key, &existing, &plan.bytes)
        .map_err(|error| format!("compact: {error:?}"))?;
    atomic_replace_locked(path, &image, actual, plan.owner).map_err(|error| error.to_string())?;
    Ok(JournalAppendProof {
        record_len: plan.bytes.len(),
        encoded_fingerprint: plan.encoded_fingerprint,
        published,
        file_synced: true,
        landing: JournalLanding::Rewritten {
            renamed_over_journal: true,
            directory_synced: true,
        },
    })
}

/// `patience` carries the caller's thread, exactly as in [`execute_journal_append`].
pub(crate) fn execute_journal_rewrite(
    path: &Path,
    plan: &JournalRewritePlan,
    patience: JournalLockPatience,
) -> JournalRewriteResult {
    if let Some(registration) = &plan.registration {
        let mut plan = plan.clone();
        plan.registration = None;
        let path = path.to_path_buf();
        return registration
            .run(move |custody, id| {
                rewrite_owned(
                    &path,
                    &plan,
                    JournalLockPatience::Worker,
                    Some((custody, id)),
                )
            })
            .unwrap_or_else(JournalRewriteResult::Failed);
    }
    rewrite_owned(path, plan, patience, None)
}

fn rewrite_owned(
    path: &Path,
    plan: &JournalRewritePlan,
    patience: JournalLockPatience,
    custody: Option<(&mut owner::Custody, u64)>,
) -> JournalRewriteResult {
    if let Err(message) = retire_owed_legacy(plan.retire_legacy.as_ref(), patience) {
        return JournalRewriteResult::Failed(message);
    }
    let result = owner::operate(path, plan.key, patience, custody, || {
        let existing = read_existing_journal(path, "preflight")?;
        let actual = JournalImageGeneration::of(&existing);
        if actual.fingerprint == plan.fingerprint {
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
    let (bytes, tail) = encode_journal_image(&records)?;
    let fingerprint = tail.fingerprint();
    Ok(JournalRewritePlan {
        registration: None,
        generation,
        key,
        base_durable,
        expected_image,
        target_seq: current.seq,
        target_text: current.text.clone(),
        bytes: Arc::from(bytes),
        fingerprint,
        tail,
        retire_legacy: None,
        owner: None,
    })
}

/// The one name every test draft root in this process hangs under.
#[cfg(test)]
const TEST_DRAFT_ROOT_PREFIX: &str = "aterm-test-drafts-";

/// aterm's shared per-user state root — the ONE rule ([`aterm_types::dirs::state_dir`]), so the journal, the harness and the session identities never disagree about the directory.
#[cfg(not(test))]
fn default_state_root() -> Option<PathBuf> {
    aterm_types::dirs::state_dir()
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    match aterm_types::fs_restricted::create_dir_all_unshared(path, 0o700) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "recovery journal root must not be a symbolic link",
                ));
            }
        }
        Err(error) => return Err(error),
    }
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
    #[cfg(all(test, any(unix, windows)))]
    owner::check_fs_thread();
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

/// Which of a journal's files [`open_journal_file`] opens.
#[derive(Clone, Copy)]
enum JournalFile {
    /// The sibling advisory lock, made when it is missing.
    Lock,
    /// The image itself, for an append in place: never made.
    Image,
}

impl JournalFile {
    const fn creates(self) -> bool {
        matches!(self, Self::Lock)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Image => "image",
        }
    }
}

fn open_journal_lock(path: &Path) -> std::io::Result<File> {
    open_journal_file(path, JournalFile::Lock)
}

fn open_journal_image(path: &Path) -> std::io::Result<File> {
    open_journal_file(path, JournalFile::Image)
}

/// Open one of a journal's files without following or waiting on a hostile final component.
#[cfg(unix)]
fn open_journal_file(path: &Path, which: JournalFile) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(which.creates())
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(journal_not_regular(which.name()));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_journal_file(path: &Path, which: JournalFile) -> std::io::Result<File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(which.creates())
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(journal_not_regular(which.name()));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_journal_file(path: &Path, which: JournalFile) -> std::io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(journal_not_regular(which.name()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && which.creates() => {}
        Err(error) => return Err(error),
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(which.creates())
        .truncate(false)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(journal_not_regular(which.name()));
    }
    Ok(file)
}

fn with_journal_lock<T>(
    path: &Path,
    patience: JournalLockPatience,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    #[cfg(all(test, any(unix, windows)))]
    owner::check_fs_thread();
    let parent = path
        .parent()
        .ok_or_else(|| "journal has no parent directory".to_string())?;
    create_private_dir(parent).map_err(|error| format!("journal lock directory: {error}"))?;
    let lock_path = journal_lock_path(path)?;
    let _held = take_linked_journal_lock(&lock_path, path, patience)?;
    #[cfg(all(test, unix))]
    copy_held_lock_for_test(_held.file());
    operation()
}

fn take_linked_journal_lock(
    lock_path: &Path,
    path: &Path,
    patience: JournalLockPatience,
) -> Result<crate::native_document_host::HeldAdvisoryLock, String> {
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

/// Which thread takes a journal lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JournalLockPatience {
    /// The caller is the thread that owns `App`.
    EventLoop,
    /// The caller is the `aterm-native-document` worker (`native_document_queue`).
    Worker,
    /// The launch sweep and a released journal's settle (`owner`).
    #[cfg_attr(
        not(any(unix, windows)),
        expect(dead_code, reason = "nothing is swept where no image is marked")
    )]
    Sweep,
}

impl JournalLockPatience {
    const fn budget(self) -> std::time::Duration {
        match self {
            Self::EventLoop => crate::native_document_host::WRITE_LOCK_RETRY_BUDGET,
            Self::Worker => std::time::Duration::from_secs(2),
            Self::Sweep => std::time::Duration::ZERO,
        }
    }
}

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
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
        }
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
#[path = "native_document_journal_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "native_document_journal_steps_tests.rs"]
mod steps_tests;
