// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic persistence contracts for native documents.
//!
//! This module performs no filesystem I/O. It produces generation-stamped file-save and
//! draft-append plans, then reduces host-reported outcomes only when their generation and
//! durability proof match the still-pending plan. The draft format is bounded, checksummed,
//! and fail-closed: recovery never returns a valid prefix when a later record is corrupt.

#![allow(
    dead_code,
    reason = "native document host-effect integration lands in staged consumers"
)]

use std::ops::Range;
use std::sync::Arc;

use aterm_buffer::Seq;

use crate::document_store::{DocumentId, DocumentSnapshot};

const JOURNAL_MAGIC: [u8; 4] = *b"ATDJ";
const JOURNAL_VERSION: u16 = 1;
const JOURNAL_HEADER_LEN: usize = 40;
const JOURNAL_CHECKSUM_LEN: usize = 8;

pub(crate) const MAX_RECORD_BYTES: usize = 40 * 1024 * 1024;
pub(crate) const MAX_JOURNAL_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const MAX_JOURNAL_RECORDS: usize = 65_536;
pub(crate) const MAX_JOURNAL_EDITS: usize = 4_096;
pub(crate) const MAX_INSERT_BYTES: usize = 32 * 1024 * 1024;

/// Restart-stable identity embedded in a private draft stream.
///
/// `DocumentId` is process-local and therefore cannot identify a journal after
/// relaunch. This key is derived from the canonical URI with a stable encoding;
/// the URI itself is never written into the draft directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct JournalDocumentKey(pub(crate) u64);

impl JournalDocumentKey {
    pub(crate) fn for_canonical_uri(uri: &str) -> Self {
        Self(ContentFingerprint::of(uri.as_bytes()).0.max(1))
    }
}

/// Stable, deterministic, non-cryptographic content fingerprint.
///
/// This is for conflict detection and effect correlation, not authenticity. Security UI
/// must never present it as a signature or cryptographic digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ContentFingerprint(pub(crate) u64);

impl ContentFingerprint {
    pub(crate) fn of(bytes: &[u8]) -> Self {
        // FNV-1a with a final length mix. Unlike DefaultHasher, this encoding is stable
        // across processes and Rust releases, which a durable journal requires.
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= bytes.len() as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        Self(hash)
    }
}

/// Version observed by the host from one target identity and its current bytes.
///
/// `identity` is an opaque platform file identity when the host can supply one. `modified_ns`
/// is an observation token, not wall-clock time used for ordering. Content remains decisive
/// even on filesystems with coarse timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObservedFileVersion {
    pub(crate) exists: bool,
    pub(crate) identity: Option<u128>,
    pub(crate) modified_ns: Option<u64>,
    pub(crate) len: u64,
    pub(crate) content: ContentFingerprint,
}

impl ObservedFileVersion {
    pub(crate) fn missing() -> Self {
        Self {
            exists: false,
            identity: None,
            modified_ns: None,
            len: 0,
            content: ContentFingerprint::of(&[]),
        }
    }

    pub(crate) fn observed(bytes: &[u8], identity: Option<u128>, modified_ns: Option<u64>) -> Self {
        Self {
            exists: true,
            identity,
            modified_ns,
            len: bytes.len() as u64,
            content: ContentFingerprint::of(bytes),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VersionConflict {
    Existence,
    Identity,
    Content,
    Metadata,
}

/// Conservatively classify a target observation against the version a save was based on.
pub(crate) fn detect_version_conflict(
    expected: ObservedFileVersion,
    actual: ObservedFileVersion,
) -> Option<VersionConflict> {
    if expected.exists != actual.exists {
        return Some(VersionConflict::Existence);
    }
    if !expected.exists {
        return None;
    }
    if matches!((expected.identity, actual.identity), (Some(a), Some(b)) if a != b) {
        return Some(VersionConflict::Identity);
    }
    if expected.len != actual.len || expected.content != actual.content {
        return Some(VersionConflict::Content);
    }
    if expected.modified_ns != actual.modified_ns {
        return Some(VersionConflict::Metadata);
    }
    None
}

fn content_is_equivalent(expected: ObservedFileVersion, actual: ObservedFileVersion) -> bool {
    expected.exists == actual.exists
        && (!expected.exists || (expected.len == actual.len && expected.content == actual.content))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileWatchInput {
    pub(crate) baseline: ObservedFileVersion,
    pub(crate) observed: ObservedFileVersion,
    pub(crate) document_dirty: bool,
    pub(crate) save_in_flight: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileWatchReduction {
    Unchanged,
    /// The host observed the exact same content through the still-valid file
    /// capability, but the target's identity or metadata token advanced (for
    /// example, an atomic replace with byte-identical contents). Rebind only
    /// the save baseline; a dirty in-memory draft must remain untouched.
    RebindEquivalent {
        change: VersionConflict,
    },
    /// The canonical document is clean, so the observed bytes may atomically
    /// replace it and become the new save baseline.
    ReloadClean {
        change: VersionConflict,
    },
    /// Unsaved local bytes win until an explicit reload/discard decision.
    ConflictDirty {
        change: VersionConflict,
    },
    /// A save owns the baseline transition. Watch observations are retried only
    /// after its proof is reduced.
    DeferredSaving {
        change: VersionConflict,
    },
}

/// Stateless file-watch reducer shared by polling, platform notifications, and
/// explicit refresh. Event duplication and ordering cannot change its verdict.
pub(crate) fn reduce_file_watch(input: FileWatchInput) -> FileWatchReduction {
    let Some(change) = detect_version_conflict(input.baseline, input.observed) else {
        return FileWatchReduction::Unchanged;
    };
    if input.save_in_flight {
        FileWatchReduction::DeferredSaving { change }
    } else if content_is_equivalent(input.baseline, input.observed) {
        // `detect_version_conflict` still makes identity/metadata decisive for
        // a save CAS. This narrower watch verdict is safe only after the host
        // has revalidated the existing path/symlink capability, and it never
        // authorizes a write by itself.
        FileWatchReduction::RebindEquivalent { change }
    } else if input.document_dirty {
        FileWatchReduction::ConflictDirty { change }
    } else {
        FileWatchReduction::ReloadClean { change }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SaveGeneration(pub(crate) u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavePlan {
    pub(crate) document: DocumentId,
    pub(crate) generation: SaveGeneration,
    pub(crate) seq: Seq,
    pub(crate) expected: ObservedFileVersion,
    pub(crate) desired: ContentFingerprint,
    pub(crate) bytes: Arc<[u8]>,
}

impl SavePlan {
    /// Host-side preflight immediately before creating/replacing the target.
    pub(crate) fn preflight(&self, actual: ObservedFileVersion) -> Result<(), VersionConflict> {
        detect_version_conflict(self.expected, actual).map_or(Ok(()), Err)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtomicSaveStage {
    Preflight,
    CreateTemporary,
    WriteTemporary,
    SyncTemporary,
    RenameTarget,
    SyncDirectory,
    ObserveCommitted,
    VerifyProof,
}

/// Host assertion supplied only after the atomic replacement protocol completes.
///
/// The booleans intentionally make partial implementations fail closed in the reducer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AtomicSaveProof {
    pub(crate) observed: ObservedFileVersion,
    pub(crate) temporary_synced: bool,
    pub(crate) renamed_over_target: bool,
    pub(crate) directory_synced: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AtomicSaveResult {
    Committed(AtomicSaveProof),
    Conflict {
        observed: ObservedFileVersion,
        /// The host revalidated the original logical path/symlink capability
        /// after observing this conflict. Only such a conflict may use the
        /// byte-equivalent baseline-rebind path.
        equivalent_rebind_allowed: bool,
    },
    Failed {
        stage: AtomicSaveStage,
        message: String,
    },
    /// Replacement may already be visible, but the host could not complete its
    /// durability/content proof. The reducer must retain the old baseline and
    /// refuse another save until an explicit disk observation reconciles it.
    PublishedUnverified {
        stage: AtomicSaveStage,
        observed: Option<ObservedFileVersion>,
        message: String,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DurableSource {
    AtomicFile,
    StableFileObservation,
    DraftJournal,
}

/// The only reducer output that may advance `DocumentStore::checkpoint_ack`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DurableCheckpoint {
    pub(crate) document: DocumentId,
    pub(crate) seq: Seq,
    pub(crate) source: DurableSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SavePhase {
    Idle,
    Saving {
        generation: SaveGeneration,
        seq: Seq,
    },
    Saved {
        seq: Seq,
    },
    Conflict {
        expected: ObservedFileVersion,
        actual: ObservedFileVersion,
        kind: VersionConflict,
    },
    Failed {
        stage: AtomicSaveStage,
        message: String,
    },
    ReconcileRequired {
        expected: ObservedFileVersion,
        observed: Option<ObservedFileVersion>,
        stage: AtomicSaveStage,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveError {
    WrongDocument,
    GenerationExhausted,
    SaveInFlight,
    ConflictRequiresReconcile,
    ReconcileRequired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveReduction {
    Durable(DurableCheckpoint),
    /// Preflight observed the same disk bytes through a newer regular-file
    /// generation. The baseline is rebound and the draft remains dirty; the
    /// caller may immediately offer a fresh, generation-bound Save.
    ReboundEquivalent(VersionConflict),
    Conflict(VersionConflict),
    Failed {
        stage: AtomicSaveStage,
        message: String,
    },
    ReconcileRequired {
        expected: ObservedFileVersion,
        observed: Option<ObservedFileVersion>,
        stage: AtomicSaveStage,
        message: String,
    },
    Cancelled,
    Stale,
}

/// Pure state reducer for one document's atomic file saves.
#[derive(Clone, Debug)]
pub(crate) struct SaveReducer {
    document: DocumentId,
    next_generation: u64,
    observed: ObservedFileVersion,
    pending: Option<SavePlan>,
    phase: SavePhase,
}

impl SaveReducer {
    pub(crate) fn new(document: DocumentId, observed: ObservedFileVersion) -> Self {
        Self {
            document,
            next_generation: 1,
            observed,
            pending: None,
            phase: SavePhase::Idle,
        }
    }

    pub(crate) fn phase(&self) -> &SavePhase {
        &self.phase
    }

    pub(crate) fn observed(&self) -> ObservedFileVersion {
        self.observed
    }

    pub(crate) fn pending(&self) -> Option<&SavePlan> {
        self.pending.as_ref()
    }

    pub(crate) fn begin(&mut self, snapshot: &DocumentSnapshot) -> Result<SavePlan, SaveError> {
        if snapshot.id != self.document {
            return Err(SaveError::WrongDocument);
        }
        if matches!(self.phase, SavePhase::Conflict { .. }) {
            return Err(SaveError::ConflictRequiresReconcile);
        }
        if matches!(self.phase, SavePhase::ReconcileRequired { .. }) {
            return Err(SaveError::ReconcileRequired);
        }
        let generation = SaveGeneration(self.allocate_generation()?);
        let bytes: Arc<[u8]> = Arc::from(snapshot.text.as_bytes());
        let plan = SavePlan {
            document: self.document,
            generation,
            seq: snapshot.seq,
            expected: self.observed,
            desired: ContentFingerprint::of(&bytes),
            bytes,
        };
        self.pending = Some(plan.clone());
        self.phase = SavePhase::Saving {
            generation,
            seq: snapshot.seq,
        };
        Ok(plan)
    }

    /// Accept an explicitly chosen external version (for reload or overwrite conflict UI).
    /// A live save cannot silently have its baseline replaced.
    pub(crate) fn accept_observation(
        &mut self,
        observed: ObservedFileVersion,
    ) -> Result<(), SaveError> {
        if self.pending.is_some() {
            return Err(SaveError::SaveInFlight);
        }
        self.observed = observed;
        self.phase = SavePhase::Idle;
        Ok(())
    }

    pub(crate) fn complete(
        &mut self,
        generation: SaveGeneration,
        result: AtomicSaveResult,
    ) -> SaveReduction {
        let Some(plan) = self.pending.as_ref() else {
            return SaveReduction::Stale;
        };
        if plan.generation != generation {
            return SaveReduction::Stale;
        }
        let plan = plan.clone();
        self.pending = None;

        match result {
            AtomicSaveResult::Committed(proof) => {
                let proof_complete = proof.temporary_synced
                    && proof.renamed_over_target
                    && proof.directory_synced
                    && proof.observed.exists
                    && proof.observed.len == plan.bytes.len() as u64
                    && proof.observed.content == plan.desired;
                if !proof_complete {
                    return self.fail(
                        AtomicSaveStage::VerifyProof,
                        "atomic save proof did not match the pending bytes".to_string(),
                    );
                }
                self.observed = proof.observed;
                self.phase = SavePhase::Saved { seq: plan.seq };
                SaveReduction::Durable(DurableCheckpoint {
                    document: self.document,
                    seq: plan.seq,
                    source: DurableSource::AtomicFile,
                })
            }
            AtomicSaveResult::Conflict {
                observed,
                equivalent_rebind_allowed,
            } => {
                let detected = detect_version_conflict(plan.expected, observed);
                if !equivalent_rebind_allowed {
                    let kind = detected.unwrap_or(VersionConflict::Identity);
                    self.phase = SavePhase::Conflict {
                        expected: plan.expected,
                        actual: observed,
                        kind,
                    };
                    return SaveReduction::Conflict(kind);
                }
                let Some(kind) = detected else {
                    return self.fail(
                        AtomicSaveStage::Preflight,
                        "host reported a conflict for an unchanged target".to_string(),
                    );
                };
                if content_is_equivalent(plan.expected, observed) {
                    self.observed = observed;
                    self.phase = SavePhase::Idle;
                    return SaveReduction::ReboundEquivalent(kind);
                }
                self.phase = SavePhase::Conflict {
                    expected: plan.expected,
                    actual: observed,
                    kind,
                };
                SaveReduction::Conflict(kind)
            }
            AtomicSaveResult::Failed { stage, message } => self.fail(stage, message),
            AtomicSaveResult::PublishedUnverified {
                stage,
                observed,
                message,
            } => {
                self.phase = SavePhase::ReconcileRequired {
                    expected: plan.expected,
                    observed,
                    stage,
                    message: message.clone(),
                };
                SaveReduction::ReconcileRequired {
                    expected: plan.expected,
                    observed,
                    stage,
                    message,
                }
            }
            AtomicSaveResult::Cancelled => {
                self.phase = SavePhase::Idle;
                SaveReduction::Cancelled
            }
        }
    }

    fn allocate_generation(&mut self) -> Result<u64, SaveError> {
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(SaveError::GenerationExhausted)?;
        Ok(generation)
    }

    fn fail(&mut self, stage: AtomicSaveStage, message: String) -> SaveReduction {
        self.phase = SavePhase::Failed {
            stage,
            message: message.clone(),
        };
        SaveReduction::Failed { stage, message }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JournalEdit {
    pub(crate) range: Range<usize>,
    pub(crate) insert: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalPayload {
    Snapshot(String),
    Delta(Vec<JournalEdit>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JournalRecord {
    /// Raw stable process document identity. Decoding never mints a `DocumentId`.
    document_raw: u64,
    pub(crate) base_seq: Seq,
    pub(crate) seq: Seq,
    pub(crate) payload: JournalPayload,
}

impl JournalRecord {
    pub(crate) fn snapshot(snapshot: &DocumentSnapshot) -> Self {
        Self::snapshot_for(JournalDocumentKey(snapshot.id.get()), snapshot)
    }

    pub(crate) fn snapshot_for(key: JournalDocumentKey, snapshot: &DocumentSnapshot) -> Self {
        Self {
            document_raw: key.0,
            base_seq: snapshot.seq,
            seq: snapshot.seq,
            payload: JournalPayload::Snapshot(snapshot.text.to_string()),
        }
    }

    pub(crate) fn delta(
        document: DocumentId,
        base_seq: Seq,
        seq: Seq,
        edits: Vec<JournalEdit>,
    ) -> Result<Self, JournalError> {
        Self::delta_for(JournalDocumentKey(document.get()), base_seq, seq, edits)
    }

    pub(crate) fn delta_for(
        key: JournalDocumentKey,
        base_seq: Seq,
        seq: Seq,
        edits: Vec<JournalEdit>,
    ) -> Result<Self, JournalError> {
        let record = Self {
            document_raw: key.0,
            base_seq,
            seq,
            payload: JournalPayload::Delta(edits),
        };
        validate_record_shape(&record)?;
        Ok(record)
    }

    pub(crate) fn belongs_to(&self, document: DocumentId) -> bool {
        self.belongs_to_key(JournalDocumentKey(document.get()))
    }

    pub(crate) fn belongs_to_key(&self, key: JournalDocumentKey) -> bool {
        self.document_raw == key.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalError {
    Empty,
    TooLarge,
    TooManyRecords,
    TooManyEdits,
    BadMagic,
    UnsupportedVersion,
    Truncated,
    LengthMismatch,
    ChecksumMismatch,
    UnknownRecordKind,
    InvalidDocument,
    WrongDocument,
    InvalidSequence,
    MissingSnapshot,
    InvalidUtf8,
    InvalidEdit,
    NumericOverflow,
}

/// Encode a single independently checksummed append record.
pub(crate) fn encode_journal_record(record: &JournalRecord) -> Result<Vec<u8>, JournalError> {
    validate_record_shape(record)?;
    let (kind, payload) = encode_payload(&record.payload)?;
    let total_len = JOURNAL_HEADER_LEN
        .checked_add(payload.len())
        .and_then(|len| len.checked_add(JOURNAL_CHECKSUM_LEN))
        .ok_or(JournalError::TooLarge)?;
    if total_len > MAX_RECORD_BYTES || total_len > u32::MAX as usize {
        return Err(JournalError::TooLarge);
    }
    let payload_len = u32::try_from(payload.len()).map_err(|_| JournalError::TooLarge)?;
    let mut encoded = Vec::with_capacity(total_len);
    encoded.extend_from_slice(&JOURNAL_MAGIC);
    put_u16(&mut encoded, JOURNAL_VERSION);
    encoded.push(kind);
    encoded.push(0);
    put_u32(&mut encoded, total_len as u32);
    put_u64(&mut encoded, record.document_raw);
    put_u64(&mut encoded, record.base_seq.0);
    put_u64(&mut encoded, record.seq.0);
    put_u32(&mut encoded, payload_len);
    debug_assert_eq!(encoded.len(), JOURNAL_HEADER_LEN);
    encoded.extend_from_slice(&payload);
    let checksum = ContentFingerprint::of(&encoded).0;
    put_u64(&mut encoded, checksum);
    Ok(encoded)
}

/// Encode a complete bounded journal image. Hosts normally append each returned record.
pub(crate) fn encode_journal(records: &[JournalRecord]) -> Result<Vec<u8>, JournalError> {
    if records.len() > MAX_JOURNAL_RECORDS {
        return Err(JournalError::TooManyRecords);
    }
    let mut encoded = Vec::new();
    for record in records {
        let next = encode_journal_record(record)?;
        let len = encoded
            .len()
            .checked_add(next.len())
            .ok_or(JournalError::TooLarge)?;
        if len > MAX_JOURNAL_BYTES {
            return Err(JournalError::TooLarge);
        }
        encoded.extend_from_slice(&next);
    }
    Ok(encoded)
}

/// Decode every record or return one error. No valid-prefix result escapes corruption.
pub(crate) fn decode_journal(bytes: &[u8]) -> Result<Vec<JournalRecord>, JournalError> {
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(JournalError::TooLarge);
    }
    if bytes.is_empty() {
        return Err(JournalError::Empty);
    }
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if records.len() == MAX_JOURNAL_RECORDS {
            return Err(JournalError::TooManyRecords);
        }
        let (record, consumed) = decode_record(&bytes[offset..])?;
        offset = offset
            .checked_add(consumed)
            .ok_or(JournalError::NumericOverflow)?;
        records.push(record);
    }
    if offset != bytes.len() {
        return Err(JournalError::LengthMismatch);
    }
    Ok(records)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JournalRecovery {
    pub(crate) document: DocumentId,
    pub(crate) durable_seq: Seq,
    pub(crate) text: String,
    pub(crate) base_content: ContentFingerprint,
    pub(crate) record_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredJournalRecovery {
    pub(crate) durable_seq: Seq,
    pub(crate) text: String,
    pub(crate) base_content: ContentFingerprint,
    pub(crate) record_count: usize,
}

/// Validate and replay a complete journal, yielding only its latest durable sequence.
pub(crate) fn recover_journal(
    document: DocumentId,
    bytes: &[u8],
) -> Result<JournalRecovery, JournalError> {
    let recovered = recover_journal_for(JournalDocumentKey(document.get()), bytes)?;
    Ok(JournalRecovery {
        document,
        durable_seq: recovered.durable_seq,
        text: recovered.text,
        base_content: recovered.base_content,
        record_count: recovered.record_count,
    })
}

/// Validate and replay using the restart-stable canonical-document key.
pub(crate) fn recover_journal_for(
    key: JournalDocumentKey,
    bytes: &[u8],
) -> Result<StoredJournalRecovery, JournalError> {
    let records = decode_journal(bytes)?;
    let mut text: Option<String> = None;
    let mut base_content = None;
    let mut durable_seq = Seq(0);
    for record in &records {
        if !record.belongs_to_key(key) {
            return Err(JournalError::WrongDocument);
        }
        match &record.payload {
            JournalPayload::Snapshot(snapshot) => {
                if text.is_some() && record.seq < durable_seq {
                    return Err(JournalError::InvalidSequence);
                }
                if base_content.is_none() {
                    base_content = Some(ContentFingerprint::of(snapshot.as_bytes()));
                }
                text = Some(snapshot.clone());
                durable_seq = record.seq;
            }
            JournalPayload::Delta(edits) => {
                let current = text.as_mut().ok_or(JournalError::MissingSnapshot)?;
                if record.base_seq != durable_seq
                    || record.seq.0
                        != record
                            .base_seq
                            .0
                            .checked_add(1)
                            .ok_or(JournalError::InvalidSequence)?
                {
                    return Err(JournalError::InvalidSequence);
                }
                apply_journal_edits(current, edits)?;
                durable_seq = record.seq;
            }
        }
    }
    Ok(StoredJournalRecovery {
        durable_seq,
        text: text.ok_or(JournalError::MissingSnapshot)?,
        base_content: base_content.ok_or(JournalError::MissingSnapshot)?,
        record_count: records.len(),
    })
}

/// Bound an append stream without losing either its disk-baseline witness or
/// its latest durable draft. The first snapshot is retained for conflict
/// classification; all intermediate deltas/snapshots collapse into one latest
/// snapshot. The returned image is independently re-encoded and checksummed.
pub(crate) fn compact_journal(
    key: JournalDocumentKey,
    bytes: &[u8],
    record_limit: usize,
    byte_limit: usize,
) -> Result<Vec<u8>, JournalError> {
    let records = decode_journal(bytes)?;
    if records.len() <= record_limit && bytes.len() <= byte_limit {
        return Ok(bytes.to_vec());
    }
    let recovered = recover_journal_for(key, bytes)?;
    let first = records
        .iter()
        .find_map(|record| match &record.payload {
            JournalPayload::Snapshot(text) => Some(JournalRecord {
                document_raw: key.0,
                base_seq: record.seq,
                seq: record.seq,
                payload: JournalPayload::Snapshot(text.clone()),
            }),
            JournalPayload::Delta(_) => None,
        })
        .ok_or(JournalError::MissingSnapshot)?;
    if first.seq == recovered.durable_seq
        && matches!(&first.payload, JournalPayload::Snapshot(text) if text == &recovered.text)
    {
        return encode_journal(&[first]);
    }
    let latest = JournalRecord {
        document_raw: key.0,
        base_seq: recovered.durable_seq,
        seq: recovered.durable_seq,
        payload: JournalPayload::Snapshot(recovered.text),
    };
    encode_journal(&[first, latest])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct JournalGeneration(pub(crate) u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JournalAppendPlan {
    pub(crate) document: DocumentId,
    pub(crate) generation: JournalGeneration,
    pub(crate) base_durable: Seq,
    pub(crate) target_seq: Seq,
    /// Exact durable journal image observed by the host that scheduled this
    /// plan. `None` is a pure reducer plan and must be bound before filesystem I/O.
    pub(crate) expected_image: Option<ContentFingerprint>,
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) encoded_fingerprint: ContentFingerprint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JournalAppendProof {
    pub(crate) appended_len: usize,
    pub(crate) encoded_fingerprint: ContentFingerprint,
    pub(crate) published_image: ContentFingerprint,
    pub(crate) file_synced: bool,
    pub(crate) renamed_over_journal: bool,
    pub(crate) directory_synced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JournalStage {
    Encode,
    Append,
    Sync,
    VerifyProof,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalAppendResult {
    Committed(JournalAppendProof),
    Failed {
        stage: JournalStage,
        message: String,
    },
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalPhase {
    Idle,
    Appending {
        generation: JournalGeneration,
        target_seq: Seq,
    },
    Durable {
        seq: Seq,
    },
    Failed {
        stage: JournalStage,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalPlanError {
    WrongDocument,
    Busy,
    AlreadyDurable,
    BaseNotDurable { expected: Seq, actual: Seq },
    GenerationExhausted,
    Record(JournalError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JournalReduction {
    Durable(DurableCheckpoint),
    Failed {
        stage: JournalStage,
        message: String,
    },
    Cancelled,
    Stale,
}

/// Serialized pure reducer for one append-only draft stream.
#[derive(Clone, Debug)]
pub(crate) struct DraftJournalReducer {
    document: DocumentId,
    key: JournalDocumentKey,
    next_generation: u64,
    durable_seq: Seq,
    pending: Option<JournalAppendPlan>,
    phase: JournalPhase,
}

impl DraftJournalReducer {
    pub(crate) fn new(document: DocumentId, durable_seq: Seq) -> Self {
        Self::new_with_key(document, JournalDocumentKey(document.get()), durable_seq)
    }

    pub(crate) fn new_with_key(
        document: DocumentId,
        key: JournalDocumentKey,
        durable_seq: Seq,
    ) -> Self {
        Self {
            document,
            key,
            next_generation: 1,
            durable_seq,
            pending: None,
            phase: JournalPhase::Idle,
        }
    }

    pub(crate) fn durable_seq(&self) -> Seq {
        self.durable_seq
    }

    pub(crate) fn phase(&self) -> &JournalPhase {
        &self.phase
    }

    pub(crate) fn plan_snapshot(
        &mut self,
        snapshot: &DocumentSnapshot,
    ) -> Result<JournalAppendPlan, JournalPlanError> {
        if snapshot.id != self.document {
            return Err(JournalPlanError::WrongDocument);
        }
        if snapshot.seq <= self.durable_seq {
            return Err(JournalPlanError::AlreadyDurable);
        }
        self.plan_record(JournalRecord::snapshot_for(self.key, snapshot))
    }

    pub(crate) fn plan_delta(
        &mut self,
        base_seq: Seq,
        seq: Seq,
        edits: Vec<JournalEdit>,
    ) -> Result<JournalAppendPlan, JournalPlanError> {
        if base_seq != self.durable_seq {
            return Err(JournalPlanError::BaseNotDurable {
                expected: self.durable_seq,
                actual: base_seq,
            });
        }
        let record = JournalRecord::delta_for(self.key, base_seq, seq, edits)
            .map_err(JournalPlanError::Record)?;
        self.plan_record(record)
    }

    pub(crate) fn complete(
        &mut self,
        generation: JournalGeneration,
        result: JournalAppendResult,
    ) -> JournalReduction {
        let Some(plan) = self.pending.as_ref() else {
            return JournalReduction::Stale;
        };
        if plan.generation != generation {
            return JournalReduction::Stale;
        }
        let plan = plan.clone();
        self.pending = None;
        match result {
            JournalAppendResult::Committed(proof) => {
                let proof_complete = proof.file_synced
                    && proof.renamed_over_journal
                    && proof.directory_synced
                    && proof.appended_len == plan.bytes.len()
                    && proof.encoded_fingerprint == plan.encoded_fingerprint;
                if !proof_complete {
                    return self.fail(
                        JournalStage::VerifyProof,
                        "draft append proof did not match the pending record".to_string(),
                    );
                }
                self.durable_seq = plan.target_seq;
                self.phase = JournalPhase::Durable {
                    seq: plan.target_seq,
                };
                JournalReduction::Durable(DurableCheckpoint {
                    document: self.document,
                    seq: plan.target_seq,
                    source: DurableSource::DraftJournal,
                })
            }
            JournalAppendResult::Failed { stage, message } => self.fail(stage, message),
            JournalAppendResult::Cancelled => {
                self.phase = JournalPhase::Idle;
                JournalReduction::Cancelled
            }
        }
    }

    fn plan_record(
        &mut self,
        record: JournalRecord,
    ) -> Result<JournalAppendPlan, JournalPlanError> {
        if self.pending.is_some() {
            return Err(JournalPlanError::Busy);
        }
        let generation = JournalGeneration(self.allocate_generation()?);
        let base_durable = self.durable_seq;
        let target_seq = record.seq;
        let encoded = encode_journal_record(&record).map_err(JournalPlanError::Record)?;
        let bytes: Arc<[u8]> = Arc::from(encoded);
        let plan = JournalAppendPlan {
            document: self.document,
            generation,
            base_durable,
            target_seq,
            expected_image: None,
            encoded_fingerprint: ContentFingerprint::of(&bytes),
            bytes,
        };
        self.pending = Some(plan.clone());
        self.phase = JournalPhase::Appending {
            generation,
            target_seq,
        };
        Ok(plan)
    }

    fn allocate_generation(&mut self) -> Result<u64, JournalPlanError> {
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(JournalPlanError::GenerationExhausted)?;
        Ok(generation)
    }

    fn fail(&mut self, stage: JournalStage, message: String) -> JournalReduction {
        self.phase = JournalPhase::Failed {
            stage,
            message: message.clone(),
        };
        JournalReduction::Failed { stage, message }
    }
}

fn validate_record_shape(record: &JournalRecord) -> Result<(), JournalError> {
    if record.document_raw == 0 {
        return Err(JournalError::InvalidDocument);
    }
    match &record.payload {
        JournalPayload::Snapshot(text) => {
            if record.base_seq != record.seq {
                return Err(JournalError::InvalidSequence);
            }
            if text.len() > MAX_INSERT_BYTES {
                return Err(JournalError::TooLarge);
            }
        }
        JournalPayload::Delta(edits) => {
            if record.seq.0
                != record
                    .base_seq
                    .0
                    .checked_add(1)
                    .ok_or(JournalError::InvalidSequence)?
            {
                return Err(JournalError::InvalidSequence);
            }
            validate_edits_shape(edits)?;
        }
    }
    Ok(())
}

fn validate_edits_shape(edits: &[JournalEdit]) -> Result<(), JournalError> {
    if edits.is_empty() || edits.len() > MAX_JOURNAL_EDITS {
        return Err(if edits.is_empty() {
            JournalError::InvalidEdit
        } else {
            JournalError::TooManyEdits
        });
    }
    let mut previous_end = 0;
    for (index, edit) in edits.iter().enumerate() {
        if edit.range.start > edit.range.end
            || (index > 0 && edit.range.start < previous_end)
            || edit.insert.len() > MAX_INSERT_BYTES
        {
            return Err(JournalError::InvalidEdit);
        }
        previous_end = edit.range.end;
    }
    Ok(())
}

fn encode_payload(payload: &JournalPayload) -> Result<(u8, Vec<u8>), JournalError> {
    match payload {
        JournalPayload::Snapshot(text) => Ok((0, text.as_bytes().to_vec())),
        JournalPayload::Delta(edits) => {
            validate_edits_shape(edits)?;
            let mut encoded = Vec::new();
            put_u32(
                &mut encoded,
                u32::try_from(edits.len()).map_err(|_| JournalError::TooManyEdits)?,
            );
            for edit in edits {
                put_u64(
                    &mut encoded,
                    u64::try_from(edit.range.start).map_err(|_| JournalError::NumericOverflow)?,
                );
                put_u64(
                    &mut encoded,
                    u64::try_from(edit.range.end).map_err(|_| JournalError::NumericOverflow)?,
                );
                put_u32(
                    &mut encoded,
                    u32::try_from(edit.insert.len()).map_err(|_| JournalError::TooLarge)?,
                );
                encoded.extend_from_slice(edit.insert.as_bytes());
            }
            Ok((1, encoded))
        }
    }
}

fn decode_record(bytes: &[u8]) -> Result<(JournalRecord, usize), JournalError> {
    if bytes.len() < JOURNAL_HEADER_LEN + JOURNAL_CHECKSUM_LEN {
        return Err(JournalError::Truncated);
    }
    if bytes[..4] != JOURNAL_MAGIC {
        return Err(JournalError::BadMagic);
    }
    let mut header = Reader::new(&bytes[4..JOURNAL_HEADER_LEN]);
    if header.u16()? != JOURNAL_VERSION {
        return Err(JournalError::UnsupportedVersion);
    }
    let kind = header.u8()?;
    if header.u8()? != 0 {
        return Err(JournalError::UnsupportedVersion);
    }
    let total_len = header.u32()? as usize;
    let document_raw = header.u64()?;
    let base_seq = Seq(header.u64()?);
    let seq = Seq(header.u64()?);
    let payload_len = header.u32()? as usize;
    if total_len > MAX_RECORD_BYTES {
        return Err(JournalError::TooLarge);
    }
    if total_len < JOURNAL_HEADER_LEN + JOURNAL_CHECKSUM_LEN || total_len > bytes.len() {
        return Err(JournalError::Truncated);
    }
    let expected_len = JOURNAL_HEADER_LEN
        .checked_add(payload_len)
        .and_then(|len| len.checked_add(JOURNAL_CHECKSUM_LEN))
        .ok_or(JournalError::LengthMismatch)?;
    if total_len != expected_len {
        return Err(JournalError::LengthMismatch);
    }
    let checksum_at = total_len - JOURNAL_CHECKSUM_LEN;
    let mut checksum_reader = Reader::new(&bytes[checksum_at..total_len]);
    let stored_checksum = checksum_reader.u64()?;
    if stored_checksum != ContentFingerprint::of(&bytes[..checksum_at]).0 {
        return Err(JournalError::ChecksumMismatch);
    }
    let payload_bytes = &bytes[JOURNAL_HEADER_LEN..checksum_at];
    let payload = match kind {
        0 => JournalPayload::Snapshot(
            std::str::from_utf8(payload_bytes)
                .map_err(|_| JournalError::InvalidUtf8)?
                .to_string(),
        ),
        1 => JournalPayload::Delta(decode_edits(payload_bytes)?),
        _ => return Err(JournalError::UnknownRecordKind),
    };
    let record = JournalRecord {
        document_raw,
        base_seq,
        seq,
        payload,
    };
    validate_record_shape(&record)?;
    Ok((record, total_len))
}

fn decode_edits(bytes: &[u8]) -> Result<Vec<JournalEdit>, JournalError> {
    let mut reader = Reader::new(bytes);
    let count = reader.u32()? as usize;
    if count == 0 || count > MAX_JOURNAL_EDITS {
        return Err(if count == 0 {
            JournalError::InvalidEdit
        } else {
            JournalError::TooManyEdits
        });
    }
    let mut edits = Vec::with_capacity(count);
    for _ in 0..count {
        let start = usize::try_from(reader.u64()?).map_err(|_| JournalError::NumericOverflow)?;
        let end = usize::try_from(reader.u64()?).map_err(|_| JournalError::NumericOverflow)?;
        let len = reader.u32()? as usize;
        if len > MAX_INSERT_BYTES {
            return Err(JournalError::TooLarge);
        }
        let insert = std::str::from_utf8(reader.bytes(len)?)
            .map_err(|_| JournalError::InvalidUtf8)?
            .to_string();
        edits.push(JournalEdit {
            range: start..end,
            insert,
        });
    }
    if !reader.is_empty() {
        return Err(JournalError::LengthMismatch);
    }
    validate_edits_shape(&edits)?;
    Ok(edits)
}

fn apply_journal_edits(text: &mut String, edits: &[JournalEdit]) -> Result<(), JournalError> {
    validate_edits_shape(edits)?;
    for edit in edits {
        if edit.range.end > text.len()
            || !text.is_char_boundary(edit.range.start)
            || !text.is_char_boundary(edit.range.end)
        {
            return Err(JournalError::InvalidEdit);
        }
    }
    for edit in edits.iter().rev() {
        text.replace_range(edit.range.clone(), &edit.insert);
    }
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn bytes(&mut self, len: usize) -> Result<&'a [u8], JournalError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(JournalError::NumericOverflow)?;
        let result = self
            .bytes
            .get(self.offset..end)
            .ok_or(JournalError::Truncated)?;
        self.offset = end;
        Ok(result)
    }

    fn u8(&mut self) -> Result<u8, JournalError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, JournalError> {
        let bytes: [u8; 2] = self
            .bytes(2)?
            .try_into()
            .map_err(|_| JournalError::Truncated)?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, JournalError> {
        let bytes: [u8; 4] = self
            .bytes(4)?
            .try_into()
            .map_err(|_| JournalError::Truncated)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, JournalError> {
        let bytes: [u8; 8] = self
            .bytes(8)?
            .try_into()
            .map_err(|_| JournalError::Truncated)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_store::{DocumentStore, DocumentTxnOutcome, TextEdit};
    use aterm_spec::derive::{config_file_commit_cas_model, native_file_watch_model};
    use aterm_spec::interp::admits;

    fn snapshot(text: &str) -> (DocumentStore, DocumentId, DocumentSnapshot) {
        let mut store = DocumentStore::new();
        let id = store.open("file:///draft.md".to_string(), text.to_string());
        let snapshot = store.snapshot(id).expect("document snapshot");
        (store, id, snapshot)
    }

    fn changed_snapshot(
        store: &mut DocumentStore,
        id: DocumentId,
        prior: &DocumentSnapshot,
        range: Range<usize>,
        insert: &str,
    ) -> DocumentSnapshot {
        let outcome = store.transact(
            id,
            prior.seq,
            vec![TextEdit {
                range,
                insert: insert.to_string(),
            }],
        );
        assert!(matches!(outcome, DocumentTxnOutcome::Committed { .. }));
        store.snapshot(id).expect("updated snapshot")
    }

    fn save_proof(plan: &SavePlan, stamp: u64) -> AtomicSaveProof {
        AtomicSaveProof {
            observed: ObservedFileVersion::observed(&plan.bytes, Some(7), Some(stamp)),
            temporary_synced: true,
            renamed_over_target: true,
            directory_synced: true,
        }
    }

    fn append_proof(plan: &JournalAppendPlan) -> JournalAppendProof {
        JournalAppendProof {
            appended_len: plan.bytes.len(),
            encoded_fingerprint: plan.encoded_fingerprint,
            published_image: ContentFingerprint::of(&plan.bytes),
            file_synced: true,
            renamed_over_journal: true,
            directory_synced: true,
        }
    }

    #[test]
    fn fingerprint_is_stable_and_content_sensitive() {
        assert_eq!(
            ContentFingerprint::of(b"same"),
            ContentFingerprint::of(b"same")
        );
        assert_ne!(
            ContentFingerprint::of(b"same"),
            ContentFingerprint::of(b"same\0")
        );
        assert_ne!(ContentFingerprint::of(b"ab"), ContentFingerprint::of(b"ba"));
    }

    #[test]
    fn conflict_detection_checks_existence_identity_content_and_mtime() {
        let base = ObservedFileVersion::observed(b"old", Some(3), Some(10));
        assert_eq!(detect_version_conflict(base, base), None);
        assert_eq!(
            detect_version_conflict(base, ObservedFileVersion::missing()),
            Some(VersionConflict::Existence)
        );
        assert_eq!(
            detect_version_conflict(
                base,
                ObservedFileVersion::observed(b"old", Some(4), Some(10))
            ),
            Some(VersionConflict::Identity)
        );
        assert_eq!(
            detect_version_conflict(
                base,
                ObservedFileVersion::observed(b"new", Some(3), Some(10))
            ),
            Some(VersionConflict::Content)
        );
        assert_eq!(
            detect_version_conflict(
                base,
                ObservedFileVersion::observed(b"old", Some(3), Some(11))
            ),
            Some(VersionConflict::Metadata)
        );
    }

    #[test]
    fn file_watch_reducer_reloads_only_clean_idle_documents_and_conforms() {
        let baseline = ObservedFileVersion::observed(b"old", Some(3), Some(10));
        let observed = ObservedFileVersion::observed(b"new", Some(3), Some(11));
        let reduce = |document_dirty, save_in_flight| {
            reduce_file_watch(FileWatchInput {
                baseline,
                observed,
                document_dirty,
                save_in_flight,
            })
        };
        assert!(matches!(
            reduce(false, false),
            FileWatchReduction::ReloadClean {
                change: VersionConflict::Content
            }
        ));
        assert!(matches!(
            reduce(true, false),
            FileWatchReduction::ConflictDirty {
                change: VersionConflict::Content
            }
        ));
        assert!(matches!(
            reduce(false, true),
            FileWatchReduction::DeferredSaving {
                change: VersionConflict::Content
            }
        ));
        assert_eq!(
            reduce_file_watch(FileWatchInput {
                baseline,
                observed: baseline,
                document_dirty: true,
                save_in_flight: true,
            }),
            FileWatchReduction::Unchanged
        );

        let equivalent_metadata = ObservedFileVersion::observed(b"old", Some(3), Some(11));
        let equivalent_identity = ObservedFileVersion::observed(b"old", Some(4), Some(11));
        for equivalent in [equivalent_metadata, equivalent_identity] {
            assert!(matches!(
                reduce_file_watch(FileWatchInput {
                    baseline,
                    observed: equivalent,
                    document_dirty: true,
                    save_in_flight: false,
                }),
                FileWatchReduction::RebindEquivalent { .. }
            ));
            assert!(matches!(
                reduce_file_watch(FileWatchInput {
                    baseline,
                    observed: equivalent,
                    document_dirty: true,
                    save_in_flight: true,
                }),
                FileWatchReduction::DeferredSaving { .. }
            ));
        }
        assert!(matches!(
            reduce_file_watch(FileWatchInput {
                baseline,
                observed,
                document_dirty: true,
                save_in_flight: false,
            }),
            FileWatchReduction::ConflictDirty {
                change: VersionConflict::Content
            }
        ));

        // Negative controls: either blind reload would lose local bytes, while
        // accepting an observation mid-save would invalidate the save proof.
        assert_ne!(reduce(true, false), reduce(false, false));
        assert_ne!(reduce(false, true), reduce(false, false));

        // Tier-1: exhaust the bounded input projection against the derived
        // priority model. This drives the shipping reducer, not a duplicate.
        let model = native_file_watch_model();
        for changed in [false, true] {
            for equivalent in [false, true] {
                for document_dirty in [false, true] {
                    for save_in_flight in [false, true] {
                        let observed = if !changed {
                            baseline
                        } else if equivalent {
                            equivalent_metadata
                        } else {
                            observed
                        };
                        let reduction = reduce_file_watch(FileWatchInput {
                            baseline,
                            observed,
                            document_dirty,
                            save_in_flight,
                        });
                        let verdict = match reduction {
                            FileWatchReduction::Unchanged => 1,
                            FileWatchReduction::ReloadClean { .. } => 2,
                            FileWatchReduction::ConflictDirty { .. } => 3,
                            FileWatchReduction::DeferredSaving { .. } => 4,
                            FileWatchReduction::RebindEquivalent { .. } => 5,
                        };
                        let mut before = model.init_state();
                        before.insert("changed", i64::from(changed));
                        before.insert("equivalent", i64::from(equivalent));
                        before.insert("dirty", i64::from(document_dirty));
                        before.insert("saving", i64::from(save_in_flight));
                        let mut after = before.clone();
                        after.insert("verdict", verdict);
                        assert_eq!(admits(&model, &before, &after), Some("Resolve"));
                        assert!(model.check_invariant("PriorityIsDeterministic", &after));
                    }
                }
            }
        }

        // Negative control: conflict cannot outrank deferral while a save owns
        // the baseline transition.
        let mut before = model.init_state();
        before.insert("changed", 1);
        before.insert("equivalent", 0);
        before.insert("dirty", 1);
        before.insert("saving", 1);
        let mut inverted = before.clone();
        inverted.insert("verdict", 3);
        assert_eq!(admits(&model, &before, &inverted), None);
        assert!(!model.check_invariant("PriorityIsDeterministic", &inverted));
    }

    #[test]
    fn save_plan_binds_generation_snapshot_and_observed_version() {
        let (_store, id, snapshot) = snapshot("draft");
        let observed = ObservedFileVersion::observed(b"prior", Some(1), Some(2));
        let mut reducer = SaveReducer::new(id, observed);
        let plan = reducer.begin(&snapshot).unwrap();
        assert_eq!(plan.generation, SaveGeneration(1));
        assert_eq!(plan.seq, snapshot.seq);
        assert_eq!(plan.expected, observed);
        assert_eq!(plan.bytes.as_ref(), b"draft");
        assert_eq!(plan.desired, ContentFingerprint::of(b"draft"));
        assert_eq!(plan.preflight(observed), Ok(()));
    }

    #[test]
    fn stale_save_completion_cannot_ack_superseding_plan() {
        let (mut store, id, first) = snapshot("a");
        let mut reducer = SaveReducer::new(id, ObservedFileVersion::missing());
        let old = reducer.begin(&first).unwrap();
        let second = changed_snapshot(&mut store, id, &first, 1..1, "b");
        let current = reducer.begin(&second).unwrap();
        assert_eq!(
            reducer.complete(
                old.generation,
                AtomicSaveResult::Committed(save_proof(&old, 1))
            ),
            SaveReduction::Stale
        );
        assert_eq!(reducer.pending(), Some(&current));
        let reduction = reducer.complete(
            current.generation,
            AtomicSaveResult::Committed(save_proof(&current, 2)),
        );
        assert_eq!(
            reduction,
            SaveReduction::Durable(DurableCheckpoint {
                document: id,
                seq: second.seq,
                source: DurableSource::AtomicFile,
            })
        );
    }

    #[test]
    fn incomplete_or_mismatched_atomic_proof_fails_closed() {
        let (_store, id, snapshot) = snapshot("draft");
        let mut reducer = SaveReducer::new(id, ObservedFileVersion::missing());
        let plan = reducer.begin(&snapshot).unwrap();
        let mut proof = save_proof(&plan, 1);
        proof.directory_synced = false;
        let reduced = reducer.complete(plan.generation, AtomicSaveResult::Committed(proof));
        assert!(matches!(
            reduced,
            SaveReduction::Failed {
                stage: AtomicSaveStage::VerifyProof,
                ..
            }
        ));
        assert!(!matches!(reducer.phase(), SavePhase::Saved { .. }));
    }

    #[test]
    fn published_unverified_requires_explicit_reconciliation_before_retry() {
        let (_store, id, snapshot) = snapshot("draft");
        let original = ObservedFileVersion::observed(b"base", Some(1), Some(1));
        let visible = ObservedFileVersion::observed(b"draft", Some(2), Some(2));
        let mut reducer = SaveReducer::new(id, original);
        let plan = reducer.begin(&snapshot).unwrap();
        let reduced = reducer.complete(
            plan.generation,
            AtomicSaveResult::PublishedUnverified {
                stage: AtomicSaveStage::SyncDirectory,
                observed: Some(visible),
                message: "directory sync failed".to_string(),
            },
        );
        assert!(matches!(
            reduced,
            SaveReduction::ReconcileRequired {
                expected,
                observed: Some(actual),
                stage: AtomicSaveStage::SyncDirectory,
                ..
            } if expected == original && actual == visible
        ));
        assert_eq!(
            reducer.observed(),
            original,
            "old baseline must not advance"
        );
        assert_eq!(reducer.begin(&snapshot), Err(SaveError::ReconcileRequired));

        // Tier-1 projection: the shipping reducer's distinct state is exactly
        // the model's published-but-unverified transition, and the healthy
        // model exposes the attempted retry as an explicit rejected self-loop.
        let model = config_file_commit_cas_model();
        let begun = model.successors("BeginManual", &model.init_state())[0].clone();
        let locked = model.successors("LockManual", &begun)[0].clone();
        let indeterminate = model.successors("ResolveManualIndeterminate", &locked)[0].clone();
        assert_eq!(indeterminate["manual_phase"], 5);
        assert_eq!(indeterminate["manual_committed"], 0);
        let rejected = model.successors("RetryIndeterminate", &indeterminate);
        assert_eq!(rejected, vec![indeterminate.clone()]);
        assert!(model.check_invariant("IndeterminateDoesNotClaimDurability", &indeterminate));

        reducer.accept_observation(visible).unwrap();
        assert_eq!(reducer.begin(&snapshot).unwrap().expected, visible);
    }

    #[test]
    fn external_change_becomes_explicit_conflict_until_user_accepts_observation() {
        let (_store, id, snapshot) = snapshot("ours");
        let original = ObservedFileVersion::observed(b"base", Some(1), Some(1));
        let external = ObservedFileVersion::observed(b"theirs", Some(1), Some(2));
        let mut reducer = SaveReducer::new(id, original);
        let plan = reducer.begin(&snapshot).unwrap();
        assert_eq!(
            reducer.complete(
                plan.generation,
                AtomicSaveResult::Conflict {
                    observed: external,
                    equivalent_rebind_allowed: true,
                }
            ),
            SaveReduction::Conflict(VersionConflict::Content)
        );
        assert_eq!(reducer.observed(), original);
        assert_eq!(
            reducer.begin(&snapshot),
            Err(SaveError::ConflictRequiresReconcile),
            "a stale conflict baseline cannot authorize a blind retry"
        );
        reducer.accept_observation(external).unwrap();
        assert_eq!(reducer.observed(), external);
        assert_eq!(reducer.begin(&snapshot).unwrap().expected, external);
    }

    #[test]
    fn equivalent_generation_save_conflict_rebinds_without_authorizing_stale_write() {
        let (_store, id, snapshot) = snapshot("ours");
        let original = ObservedFileVersion::observed(b"base", Some(1), Some(1));
        let replaced = ObservedFileVersion::observed(b"base", Some(2), Some(2));

        let mut unvalidated = SaveReducer::new(id, original);
        let unvalidated_plan = unvalidated.begin(&snapshot).unwrap();
        assert_eq!(
            unvalidated.complete(
                unvalidated_plan.generation,
                AtomicSaveResult::Conflict {
                    observed: original,
                    equivalent_rebind_allowed: false,
                }
            ),
            SaveReduction::Conflict(VersionConflict::Identity),
            "same bytes cannot rebind an invalidated path or symlink capability"
        );
        assert_eq!(unvalidated.observed(), original);

        let mut reducer = SaveReducer::new(id, original);
        let stale_plan = reducer.begin(&snapshot).unwrap();
        assert_eq!(
            stale_plan.preflight(replaced),
            Err(VersionConflict::Identity)
        );
        assert_eq!(
            reducer.complete(
                stale_plan.generation,
                AtomicSaveResult::Conflict {
                    observed: replaced,
                    equivalent_rebind_allowed: true,
                }
            ),
            SaveReduction::ReboundEquivalent(VersionConflict::Identity)
        );
        assert_eq!(reducer.observed(), replaced);
        assert_eq!(reducer.phase(), &SavePhase::Idle);

        let rebound_plan = reducer.begin(&snapshot).unwrap();
        assert_eq!(rebound_plan.expected, replaced);
        assert_eq!(rebound_plan.preflight(replaced), Ok(()));

        // Tier-1: the save-preflight path and watch path project the same
        // byte-equivalent rebind verdict. A true content change remains the
        // explicit-conflict negative control above.
        let model = native_file_watch_model();
        let mut before = model.init_state();
        before.insert("changed", 1);
        before.insert("equivalent", 1);
        before.insert("dirty", 1);
        before.insert("saving", 0);
        let mut after = before.clone();
        after.insert("verdict", 5);
        assert_eq!(admits(&model, &before, &after), Some("Resolve"));
        assert!(model.check_invariant("PriorityIsDeterministic", &after));

        let mut unsafe_conflict = before.clone();
        unsafe_conflict.insert("verdict", 3);
        assert_eq!(admits(&model, &before, &unsafe_conflict), None);
        assert!(!model.check_invariant("PriorityIsDeterministic", &unsafe_conflict));
    }

    #[test]
    fn record_round_trip_preserves_snapshot_and_unicode_delta() {
        let (_store, id, snapshot) = snapshot("aλc");
        let first = JournalRecord::snapshot(&snapshot);
        let second = JournalRecord::delta(
            id,
            snapshot.seq,
            Seq(snapshot.seq.0 + 1),
            vec![JournalEdit {
                range: 1..3,
                insert: "β".to_string(),
            }],
        )
        .unwrap();
        let bytes = encode_journal(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(decode_journal(&bytes).unwrap(), vec![first, second]);
    }

    #[test]
    fn recovery_replays_incremental_records_to_latest_durable_seq() {
        let (_store, id, snapshot) = snapshot("one two");
        let seq2 = Seq(snapshot.seq.0 + 1);
        let seq3 = Seq(snapshot.seq.0 + 2);
        let records = vec![
            JournalRecord::snapshot(&snapshot),
            JournalRecord::delta(
                id,
                snapshot.seq,
                seq2,
                vec![JournalEdit {
                    range: 0..3,
                    insert: "ONE".to_string(),
                }],
            )
            .unwrap(),
            JournalRecord::delta(
                id,
                seq2,
                seq3,
                vec![JournalEdit {
                    range: 7..7,
                    insert: "!".to_string(),
                }],
            )
            .unwrap(),
        ];
        let recovered = recover_journal(id, &encode_journal(&records).unwrap()).unwrap();
        assert_eq!(recovered.durable_seq, seq3);
        assert_eq!(recovered.text, "ONE two!");
        assert_eq!(recovered.record_count, 3);
    }

    #[test]
    fn checksum_corruption_rejects_the_whole_journal_not_a_valid_prefix() {
        let (_store, id, snapshot) = snapshot("draft");
        let first = JournalRecord::snapshot(&snapshot);
        let second = JournalRecord::delta(
            id,
            snapshot.seq,
            Seq(snapshot.seq.0 + 1),
            vec![JournalEdit {
                range: 5..5,
                insert: "!".to_string(),
            }],
        )
        .unwrap();
        let mut bytes = encode_journal(&[first, second]).unwrap();
        let last_payload_byte = bytes.len() - JOURNAL_CHECKSUM_LEN - 1;
        bytes[last_payload_byte] ^= 0x40;
        assert_eq!(decode_journal(&bytes), Err(JournalError::ChecksumMismatch));
        assert_eq!(
            recover_journal(id, &bytes),
            Err(JournalError::ChecksumMismatch)
        );
    }

    #[test]
    fn truncated_and_unknown_version_records_are_rejected() {
        let (_store, _id, snapshot) = snapshot("draft");
        let bytes = encode_journal_record(&JournalRecord::snapshot(&snapshot)).unwrap();
        assert_eq!(
            decode_journal(&bytes[..bytes.len() - 1]),
            Err(JournalError::Truncated)
        );
        let mut future = bytes;
        future[4..6].copy_from_slice(&(JOURNAL_VERSION + 1).to_le_bytes());
        assert_eq!(
            decode_journal(&future),
            Err(JournalError::UnsupportedVersion)
        );
    }

    #[test]
    fn recovery_rejects_wrong_document_sequence_gap_and_utf8_split() {
        let (mut store, id_a, snapshot_a) = snapshot("λx");
        let id_b = store.open("file:///other".to_string(), String::new());
        assert_eq!(
            recover_journal(
                id_b,
                &encode_journal(&[JournalRecord::snapshot(&snapshot_a)]).unwrap()
            ),
            Err(JournalError::WrongDocument)
        );

        let gap = JournalRecord {
            document_raw: id_a.get(),
            base_seq: snapshot_a.seq,
            seq: Seq(snapshot_a.seq.0 + 1),
            payload: JournalPayload::Delta(vec![JournalEdit {
                range: 1..2,
                insert: "z".to_string(),
            }]),
        };
        let bytes = encode_journal(&[JournalRecord::snapshot(&snapshot_a), gap]).unwrap();
        assert_eq!(
            recover_journal(id_a, &bytes),
            Err(JournalError::InvalidEdit)
        );

        let missing = JournalRecord::delta(
            id_a,
            Seq(snapshot_a.seq.0 + 1),
            Seq(snapshot_a.seq.0 + 2),
            vec![JournalEdit {
                range: 0..0,
                insert: "z".to_string(),
            }],
        )
        .unwrap();
        let bytes = encode_journal(&[JournalRecord::snapshot(&snapshot_a), missing]).unwrap();
        assert_eq!(
            recover_journal(id_a, &bytes),
            Err(JournalError::InvalidSequence)
        );
    }

    #[test]
    fn journal_reducer_serializes_appends_and_rejects_stale_completion() {
        let (mut store, id, first) = snapshot("a");
        let mut reducer = DraftJournalReducer::new(id, Seq(0));
        let initial = reducer.plan_snapshot(&first).unwrap();
        assert_eq!(reducer.plan_snapshot(&first), Err(JournalPlanError::Busy));
        assert_eq!(
            reducer.complete(
                JournalGeneration(initial.generation.0 + 99),
                JournalAppendResult::Committed(append_proof(&initial))
            ),
            JournalReduction::Stale
        );
        assert_eq!(reducer.durable_seq(), Seq(0));
        assert_eq!(
            reducer.complete(
                initial.generation,
                JournalAppendResult::Committed(append_proof(&initial))
            ),
            JournalReduction::Durable(DurableCheckpoint {
                document: id,
                seq: first.seq,
                source: DurableSource::DraftJournal,
            })
        );

        let second = changed_snapshot(&mut store, id, &first, 1..1, "b");
        let delta = reducer
            .plan_delta(
                first.seq,
                second.seq,
                vec![JournalEdit {
                    range: 1..1,
                    insert: "b".to_string(),
                }],
            )
            .unwrap();
        assert_eq!(delta.base_durable, first.seq);
    }

    #[test]
    fn journal_durability_requires_matching_synced_append_proof() {
        let (_store, id, snapshot) = snapshot("draft");
        let mut reducer = DraftJournalReducer::new(id, Seq(0));
        let plan = reducer.plan_snapshot(&snapshot).unwrap();
        let mut proof = append_proof(&plan);
        proof.file_synced = false;
        assert!(matches!(
            reducer.complete(plan.generation, JournalAppendResult::Committed(proof)),
            JournalReduction::Failed {
                stage: JournalStage::VerifyProof,
                ..
            }
        ));
        assert_eq!(reducer.durable_seq(), Seq(0));
    }

    #[test]
    fn empty_delta_and_oversized_edit_sets_are_bounded() {
        let (_store, id, snapshot) = snapshot("x");
        assert_eq!(
            JournalRecord::delta(id, snapshot.seq, Seq(snapshot.seq.0 + 1), vec![]),
            Err(JournalError::InvalidEdit)
        );
        let edits = (0..=MAX_JOURNAL_EDITS)
            .map(|_| JournalEdit {
                range: 0..0,
                insert: String::new(),
            })
            .collect();
        assert_eq!(
            JournalRecord::delta(id, snapshot.seq, Seq(snapshot.seq.0 + 1), edits),
            Err(JournalError::TooManyEdits)
        );
    }
}
