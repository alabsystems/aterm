// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable private crash journals for native documents.
//!
//! A journal filename is derived from the canonical URI but never contains it.
//! Every publication uses a same-directory temporary file, `sync_all`, atomic
//! rename, and directory sync. Thus an interrupted append leaves the previous
//! complete image recoverable instead of exposing a valid-prefix ambiguity.
//! Existing images and lock files are admitted through non-blocking, no-follow,
//! regular-file handles so a planted FIFO, device, or link cannot park Manual or
//! redirect persistence outside the private journal directory.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_buffer::Seq;

use crate::document_store::{DocumentId, DocumentSnapshot};
use crate::native_document_io::{
    ContentFingerprint, DraftJournalReducer, DurableCheckpoint, DurableSource, JournalAppendPlan,
    JournalAppendProof, JournalAppendResult, JournalDocumentKey, JournalEdit, JournalError,
    JournalRecord, JournalReduction, JournalStage, MAX_INSERT_BYTES, MAX_JOURNAL_BYTES,
    compact_journal, encode_journal, recover_journal_for,
};

const COMPACT_RECORDS: usize = 128;
const COMPACT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryNotice {
    Recovered { records: usize },
    DiskConflict,
    Corrupt(String),
}

impl RecoveryNotice {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Recovered { records } => format!(
                "Recovered an unsaved draft from {records} durable journal record{}",
                if *records == 1 { "" } else { "s" }
            ),
            Self::DiskConflict => {
                "A recovered draft conflicts with newer disk content; the draft was preserved"
                    .to_string()
            }
            Self::Corrupt(reason) => {
                format!("A damaged recovery journal was preserved: {reason}")
            }
        }
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JournalImageGeneration {
    exists: bool,
    fingerprint: ContentFingerprint,
}

impl JournalImageGeneration {
    fn missing() -> Self {
        Self {
            exists: false,
            fingerprint: ContentFingerprint::of(&[]),
        }
    }

    fn of(bytes: &[u8]) -> Self {
        Self {
            exists: true,
            fingerprint: ContentFingerprint::of(bytes),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InitializedJournal {
    pub(crate) key: JournalDocumentKey,
    pub(crate) path: PathBuf,
    pub(crate) durable_seq: Seq,
    pub(crate) durable_text: Arc<str>,
    pub(crate) image_fingerprint: ContentFingerprint,
    pub(crate) notice: Option<RecoveryNotice>,
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "only recovery diagnostics inspect this path")
    )]
    pub(crate) preserved_path: Option<PathBuf>,
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
}

#[derive(Clone, Debug)]
struct SavedBaseline {
    seq: Seq,
    text: Arc<str>,
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
    rewrite_inflight: Option<JournalRewritePlan>,
    pending_checkpoint: Option<SavedBaseline>,
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
}

impl DocumentJournalStore {
    pub(crate) fn system_default() -> Result<Self, String> {
        Ok(Self {
            host: DraftJournalHost::system_default()?,
            entries: BTreeMap::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(root: PathBuf) -> Result<Self, String> {
        Ok(Self {
            host: DraftJournalHost::new(root)?,
            entries: BTreeMap::new(),
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
            },
        );
        Ok(initialized)
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

    pub(crate) fn next_effect(
        &mut self,
        document: DocumentId,
    ) -> Result<Option<JournalEffect>, String> {
        let Some(entry) = self.entries.get_mut(&document) else {
            return Err("document journal was not initialized".to_string());
        };
        if entry.append_text.is_some() || entry.rewrite_inflight.is_some() {
            return Ok(None);
        }
        if let Some(saved) = entry.pending_checkpoint.take() {
            let generation = JournalRewriteGeneration(entry.next_rewrite_generation);
            entry.next_rewrite_generation = entry
                .next_rewrite_generation
                .checked_add(1)
                .ok_or_else(|| "journal checkpoint generation exhausted".to_string())?;
            let disk = synthetic_snapshot(document, saved.seq, saved.text);
            let plan = checkpoint_plan(
                entry.key,
                generation,
                entry.reducer.durable_seq(),
                entry.durable_image,
                &disk,
                &entry.desired,
            )
            .map_err(|error| format!("could not encode journal checkpoint: {error:?}"))?;
            entry.rewrite_inflight = Some(plan.clone());
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
        entry.append_text = Some(entry.desired.text.clone());
        Ok(Some(JournalEffect::Append {
            path: entry.path.clone(),
            key: entry.key,
            plan,
        }))
    }

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
                JournalCompletion::Durable {
                    document,
                    seq: checkpoint.seq,
                }
            }
            JournalReduction::Failed { stage, message } => {
                entry.append_text = None;
                JournalCompletion::Failed {
                    document,
                    message: format!("journal append failed at {stage:?}: {message}"),
                }
            }
            JournalReduction::Cancelled => {
                entry.append_text = None;
                JournalCompletion::Failed {
                    document,
                    message: "journal append was cancelled".to_string(),
                }
            }
            JournalReduction::Stale => JournalCompletion::Stale,
        }
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
        if plan.generation != generation {
            return JournalCompletion::Stale;
        }
        let plan = entry
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

    #[cfg(test)]
    pub(crate) fn durable_seq(&self, document: DocumentId) -> Option<Seq> {
        self.entries
            .get(&document)
            .map(|entry| entry.reducer.durable_seq())
    }
}

fn synthetic_snapshot(document: DocumentId, seq: Seq, text: Arc<str>) -> DocumentSnapshot {
    DocumentSnapshot {
        id: document,
        seq,
        file_version: crate::document_store::FileVersion {
            content_fingerprint: ContentFingerprint::of(text.as_bytes()).0,
        },
        text,
    }
}

fn single_edit(before: &str, after: &str) -> JournalEdit {
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

impl DraftJournalHost {
    pub(crate) fn system_default() -> Result<Self, String> {
        #[cfg(test)]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let root = std::env::temp_dir().join(format!(
                "aterm-test-drafts-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            Self::new(root)
        }
        #[cfg(not(test))]
        {
            let root = default_state_root().ok_or_else(|| {
                "could not locate a private state directory for document recovery".to_string()
            })?;
            Self::new(root.join("drafts"))
        }
    }

    pub(crate) fn new(root: PathBuf) -> Result<Self, String> {
        create_private_dir(&root).map_err(|error| error.to_string())?;
        Ok(Self { root })
    }

    pub(crate) fn inspect_open(
        &self,
        canonical_uri: &str,
        disk_bytes: &[u8],
    ) -> Result<JournalOpenDecision, String> {
        let key = JournalDocumentKey::for_canonical_uri(canonical_uri);
        let path = self.path_for(key);
        let mut decision = JournalOpenDecision {
            key,
            path: path.clone(),
            recovered_text: None,
            notice: None,
            preserve_existing: false,
            expected_image: JournalImageGeneration::missing(),
        };
        let bytes = match read_journal_image(&path) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(decision),
            Err(error) => return Err(format!("could not read recovery journal: {error}")),
        };
        decision.expected_image = JournalImageGeneration::of(&bytes);
        match recover_journal_for(key, &bytes) {
            Ok(recovered) => {
                let disk = ContentFingerprint::of(disk_bytes);
                let latest = ContentFingerprint::of(recovered.text.as_bytes());
                if recovered.base_content == disk {
                    if latest != disk {
                        decision.recovered_text = Some(recovered.text);
                        decision.notice = Some(RecoveryNotice::Recovered {
                            records: recovered.record_count,
                        });
                    }
                } else if latest != disk {
                    decision.notice = Some(RecoveryNotice::DiskConflict);
                    decision.preserve_existing = true;
                }
            }
            Err(error) => {
                decision.notice = Some(RecoveryNotice::Corrupt(format!("{error:?}")));
                decision.preserve_existing = true;
            }
        }
        Ok(decision)
    }

    /// Publish a fresh, sequence-aligned image before any document view becomes
    /// visible. A conflicting/corrupt prior image is renamed, never deleted.
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
        let preserved_path = with_journal_lock(&decision.path, || {
            let current = observe_journal_image(&decision.path)?;
            if current != decision.expected_image {
                return Err(format!(
                    "journal changed after recovery inspection (expected {:?}, found {:?}); reopen \
                     the document before initializing recovery",
                    decision.expected_image, current
                ));
            }
            let preserved = if decision.preserve_existing && current.exists {
                Some(self.preserve_locked(&decision.path, current)?)
            } else {
                None
            };
            atomic_replace_locked(&decision.path, &bytes, current)
                .map_err(|error| error.to_string())?;
            Ok(preserved)
        })?;
        Ok(InitializedJournal {
            key: decision.key,
            path: decision.path,
            durable_seq: current.seq,
            durable_text: current.text.clone(),
            image_fingerprint,
            notice: decision.notice,
            preserved_path,
        })
    }

    pub(crate) fn path_for(&self, key: JournalDocumentKey) -> PathBuf {
        self.root.join(format!("{:016x}.atdj", key.0))
    }

    fn preserve_locked(
        &self,
        path: &Path,
        expected: JournalImageGeneration,
    ) -> Result<PathBuf, String> {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("draft");
        for suffix in 1..=10_000_u32 {
            let candidate = self.root.join(format!("{stem}.preserved-{suffix}.atdj"));
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
            sync_directory(&self.root)
                .map_err(|error| format!("could not sync preserved recovery journal: {error}"))?;
            return Ok(candidate);
        }
        Err("too many preserved recovery journals for this document".to_string())
    }
}

pub(crate) fn execute_journal_append(
    path: &Path,
    key: JournalDocumentKey,
    plan: &JournalAppendPlan,
) -> JournalAppendResult {
    let result = with_journal_lock(path, || {
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
        atomic_replace_locked(path, &image, actual).map_err(|error| error.to_string())?;
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

pub(crate) fn execute_journal_rewrite(
    path: &Path,
    plan: &JournalRewritePlan,
) -> JournalRewriteResult {
    let result = with_journal_lock(path, || {
        let existing = read_existing_journal(path, "preflight")?;
        let actual = JournalImageGeneration::of(&existing);
        if actual.fingerprint == plan.fingerprint {
            // A prior attempt may have published the exact image but lost its
            // post-publication proof. Re-publishing the already verified bytes
            // avoids a second pathname open (and its final-component swap window).
            return atomic_replace_locked(path, &plan.bytes, actual)
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
        atomic_replace_locked(path, &plan.bytes, actual).map_err(|error| error.to_string())
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
    })
}

#[cfg(not(test))]
fn default_state_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("ATERM_STATE_HOME") {
        return Some(PathBuf::from(root));
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support/aterm"))
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("aterm"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
            return Some(PathBuf::from(root).join("aterm"));
        }
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".local/state/aterm"))
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    fs::create_dir_all(path)?;
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
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "journal has no parent directory".to_string())?;
    create_private_dir(parent).map_err(|error| format!("journal lock directory: {error}"))?;
    let lock_path = journal_lock_path(path)?;
    let lock = open_journal_lock(&lock_path)
        .map_err(|error| format!("open journal lock {}: {error}", lock_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect journal lock {}: {error}", lock_path.display()))?;
    }
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => format!(
            "recovery journal {} is busy; retry opening Manual or saving after the other \
                 aterm process finishes",
            path.display()
        ),
        std::fs::TryLockError::Error(error) => {
            format!("lock journal {}: {error}", path.display())
        }
    })?;
    operation()
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

fn atomic_replace_locked(
    path: &Path,
    bytes: &[u8],
    expected: JournalImageGeneration,
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
        temporary_file
            .sync_all()
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
        sync_directory(parent).map_err(JournalReplaceError::PublishedUnverified)?;
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

#[cfg(not(windows))]
fn replace_file(temporary: &Path, target: &Path, target_existed: bool) -> std::io::Result<()> {
    if target_existed {
        fs::rename(temporary, target)
    } else {
        // A normal rename would overwrite a path planted after the final
        // missing observation. Hard-linking publishes the synced inode only if
        // the destination is still absent; removing the temp leaves it visible.
        fs::hard_link(temporary, target)?;
        fs::remove_file(temporary)
    }
}

#[cfg(windows)]
fn replace_file(temporary: &Path, target: &Path, target_existed: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const REPLACEFILE_WRITE_THROUGH: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let existing = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both UTF-16 buffers are NUL-terminated and live for the call.
    let moved = unsafe {
        if target_existed {
            ReplaceFileW(
                replacement.as_ptr(),
                existing.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } else {
            MoveFileExW(
                existing.as_ptr(),
                replacement.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?
        .sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
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
        let mut store = DocumentStore::new();
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
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn inspection_refuses_fifo_and_final_symlink_without_waiting() {
        let root = test_root("special-inspection");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let canonical = "file:///tmp/special-inspection.md";
        let path = host.path_for(JournalDocumentKey::for_canonical_uri(canonical));
        make_fifo(&path);
        let started = std::time::Instant::now();
        let fifo_error = host.inspect_open(canonical, b"disk").unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
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
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
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
        let held = open_journal_lock(&lock_path).unwrap();
        held.lock().unwrap();

        let started = std::time::Instant::now();
        let error = host.initialize(decision.clone(), &disk, &disk).unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.contains("busy"), "{error}");
        assert!(error.contains("retry"), "{error}");

        drop(held);
        let initialized = host.initialize(decision, &disk, &disk).unwrap();
        assert!(recover_journal_for(initialized.key, &fs::read(initialized.path).unwrap()).is_ok());
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
        let initialized = host
            .initialize(
                host.inspect_open(canonical, disk.text.as_bytes()).unwrap(),
                &disk,
                &disk,
            )
            .unwrap();
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
            execute_journal_append(&initialized.path, initialized.key, &append),
            JournalAppendResult::Failed { message, .. }
                if message.contains("regular non-link")
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
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
            execute_journal_rewrite(&initialized.path, &rewrite),
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
        host.initialize(first, &disk, &current).unwrap();

        let reopened = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        assert_eq!(reopened.recovered_text.as_deref(), Some("base draft"));
        assert!(matches!(
            reopened.notice,
            Some(RecoveryNotice::Recovered { .. })
        ));
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
        let initialized = host.initialize(decision, &disk, &current).unwrap();
        let original = fs::read(&initialized.path).unwrap();

        let conflict = host.inspect_open(canonical, b"external").unwrap();
        assert!(matches!(
            conflict.notice,
            Some(RecoveryNotice::DiskConflict)
        ));
        let (other_store, _, external) = snapshots("external");
        let preserved = host.initialize(conflict, &external, &external).unwrap();
        assert_eq!(
            fs::read(preserved.preserved_path.unwrap()).unwrap(),
            original
        );
        drop(other_store);

        fs::write(&preserved.path, b"torn").unwrap();
        let corrupt = host.inspect_open(canonical, b"external").unwrap();
        assert!(matches!(corrupt.notice, Some(RecoveryNotice::Corrupt(_))));
        let kept = host.initialize(corrupt, &external, &external).unwrap();
        assert_eq!(fs::read(kept.preserved_path.unwrap()).unwrap(), b"torn");
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
        let initialized = host.initialize(first, &disk, &disk).unwrap();
        let before = fs::read(&initialized.path).unwrap();

        let error = host.initialize(stale, &disk, &disk).unwrap_err();
        assert!(
            error.contains("changed after recovery inspection"),
            "{error}"
        );
        assert_eq!(fs::read(&initialized.path).unwrap(), before);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_temporary_write_never_replaces_last_durable_image() {
        let root = test_root("crash");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("base");
        let canonical = "file:///tmp/draft.md";
        let initialized = host
            .initialize(
                host.inspect_open(canonical, disk.text.as_bytes()).unwrap(),
                &disk,
                &disk,
            )
            .unwrap();
        let before = fs::read(&initialized.path).unwrap();
        let abandoned = initialized.path.parent().unwrap().join(".abandoned.tmp");
        fs::write(&abandoned, &before[..before.len() / 2]).unwrap();

        assert_eq!(fs::read(&initialized.path).unwrap(), before);
        assert!(recover_journal_for(initialized.key, &before).is_ok());
        assert!(recover_journal_for(initialized.key, &fs::read(abandoned).unwrap()).is_err());
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
        let initialized = host
            .initialize(
                host.inspect_open(canonical, disk.text.as_bytes()).unwrap(),
                &disk,
                &disk,
            )
            .unwrap();
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
        journals.initialize(decision, &disk, &disk).unwrap();

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

        let committed = execute_journal_append(&path, key, &first);
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
        let result = execute_journal_append(&path, key, &latest);
        assert!(matches!(
            journals.complete_append(id, latest.generation, result),
            JournalCompletion::Durable { seq, .. } if seq == third.seq
        ));
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
    fn checkpoint_rewrite_is_proof_gated_and_keeps_newer_draft() {
        let root = test_root("checkpoint");
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("base");
        let decision = journals
            .inspect_open("file:///tmp/draft.md", disk.text.as_bytes())
            .unwrap();
        journals.initialize(decision, &disk, &disk).unwrap();
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
        let mut bad = match execute_journal_rewrite(&path, &plan) {
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
        let result = execute_journal_rewrite(&path, &plan);
        assert_eq!(
            journals.complete_rewrite(id, plan.generation, result),
            JournalCompletion::Durable {
                document: id,
                seq: newer.seq
            }
        );
        let recovered = recover_journal_for(
            JournalDocumentKey::for_canonical_uri("file:///tmp/draft.md"),
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
        let initialized = host
            .initialize(
                host.inspect_open(canonical, disk.text.as_bytes()).unwrap(),
                &disk,
                &disk,
            )
            .unwrap();
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
            execute_journal_append(&initialized.path, initialized.key, &append),
            JournalAppendResult::Committed(_)
        ));

        assert!(matches!(
            execute_journal_rewrite(&initialized.path, &rewrite),
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
        let initialized = host
            .initialize(
                host.inspect_open(canonical, disk.text.as_bytes()).unwrap(),
                &disk,
                &disk,
            )
            .unwrap();
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
            execute_journal_append(&initialized.path, initialized.key, &plan),
            JournalAppendResult::Failed { message, .. } if message.contains("image changed")
        ));
        assert_eq!(fs::read(&initialized.path).unwrap(), other_image);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cross_process_same_journal_generation_has_one_winner() {
        const CHILD: &str = "ATERM_JOURNAL_COMMIT_TEST_CHILD";
        const URI: &str = "file:///tmp/cross-process-draft.md";
        if let Ok(role) = std::env::var(CHILD) {
            let path = PathBuf::from(std::env::var("ATERM_JOURNAL_COMMIT_TEST_PATH").unwrap());
            let root = path.parent().unwrap();
            let key = JournalDocumentKey::for_canonical_uri(URI);
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
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !go.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent race timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let retry_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let verdict = loop {
                match execute_journal_append(&path, key, &plan) {
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

        let test_name = std::thread::current()
            .name()
            .expect("test harness names the current test")
            .to_string();
        let root = test_root("process-race");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("base");
        let initialized = host
            .initialize(
                host.inspect_open(URI, disk.text.as_bytes()).unwrap(),
                &disk,
                &disk,
            )
            .unwrap();
        let mut children = Vec::new();
        for role in ["one", "two"] {
            children.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .arg("--exact")
                    .arg(&test_name)
                    .arg("--nocapture")
                    .env(CHILD, role)
                    .env("ATERM_JOURNAL_COMMIT_TEST_PATH", &initialized.path)
                    .spawn()
                    .unwrap(),
            );
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while ["one", "two"]
            .iter()
            .any(|role| !root.join(format!("ready-{role}")).exists())
        {
            assert!(std::time::Instant::now() < deadline, "children timed out");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        fs::write(root.join("go"), b"go").unwrap();
        for mut child in children {
            assert!(child.wait().unwrap().success());
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
        let model = aterm_spec::derive::native_draft_journal_model();
        let edited = model.successors("Edit", &model.init_state())[0].clone();
        let begun = model.successors("BeginJournal", &edited)[0].clone();
        let external = model.successors("ExternalJournalCommit", &begun)[0].clone();
        let rejected = model.successors("RejectJournalDiskConflict", &external)[0].clone();
        assert_eq!(rejected["disk_conflict_rejected"], 1);
        assert!(model.check_invariant("JournalImageCas", &rejected));
        assert!(model.successors("AcceptJournal", &external).is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
