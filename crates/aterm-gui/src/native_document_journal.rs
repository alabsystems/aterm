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
            let disk = synthetic_snapshot(document, saved.seq, saved.text.clone());
            let plan = checkpoint_plan(
                entry.key,
                generation,
                entry.reducer.durable_seq(),
                entry.durable_image,
                &disk,
                &entry.desired,
            )
            .map_err(|error| format!("could not encode journal checkpoint: {error:?}"))?;
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
        let plan = entry
            .rewrite_inflight
            .take()
            .expect("generation checked against live rewrite")
            .plan;
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
            // roots are draft-recovery journals and they are never cleaned up,
            // so a pid+ordinal name alone let macOS pid recycling hand a fresh
            // test process a prior run's unsaved draft — which crash recovery
            // then dutifully replayed into the new test's document (the proven
            // six-tests-at-once app_documents flake: one Enter on "abc" landed
            // on a resurrected "abc\n" and read back "abc\n\n"). Stamp the
            // root with a per-process boot nonce so the name is unique per
            // process INSTANCE, and remove any same-named corpse: a live
            // process cannot share our pid AND our stamp.
            static STAMP: std::sync::OnceLock<u128> = std::sync::OnceLock::new();
            let stamp = *STAMP.get_or_init(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            });
            let root = std::env::temp_dir().join(format!(
                "aterm-test-drafts-{}-{stamp:x}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
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
    ///
    /// ALWAYS ON THE EVENT LOOP, hence no patience parameter: the one production
    /// caller is `App::open_native_document_in_window` (`app_documents.rs:2555`),
    /// which runs it SYNCHRONOUSLY inside a `&mut self` handler because the
    /// `InitializedJournal` it returns is what the open flow needs to build the
    /// view. There is nowhere to hand it: the document worker's queue carries
    /// appends and rewrites, whose results arrive later through `Wake`, and an
    /// open cannot proceed on a promise. So this path takes the frame-sized
    /// budget and, past it, REPORTS BUSY rather than blocking longer — the
    /// refusal `app_documents.rs:3143` pins, carrying the "retry opening Manual"
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
        let preserved_path = with_journal_lock(
            &decision.path,
            JournalLockPatience::EventLoop,
            || {
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
            },
        )?;
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

/// `patience` is the CALLER's declaration of which thread it is on, because that
/// is the only place the answer is known: `app_documents.rs:193` runs this on the
/// `aterm-native-document` worker, `app_documents.rs:1024` runs it inline on the
/// thread that owns `App`. See [`JournalLockPatience`].
pub(crate) fn execute_journal_append(
    path: &Path,
    key: JournalDocumentKey,
    plan: &JournalAppendPlan,
    patience: JournalLockPatience,
) -> JournalAppendResult {
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

/// `patience` carries the caller's thread, exactly as in [`execute_journal_append`].
pub(crate) fn execute_journal_rewrite(
    path: &Path,
    plan: &JournalRewritePlan,
    patience: JournalLockPatience,
) -> JournalRewriteResult {
    let result = with_journal_lock(path, patience, || {
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
    patience: JournalLockPatience,
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
    take_journal_lock(&lock, path, patience.budget())?;
    operation()
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
/// `forkpty` whose `libc::fork` is `aterm-pty/src/unix.rs:1698`, and whose child
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
///     (`aterm-pty/src/unix.rs:739-744` times fork to the `O_CLOEXEC` status
///     pipe's EOF, which IS this window): p50 2.99 ms, p99 6.0 ms, max 12.7 ms
///     over n=5000 at ordinary load; p50 4.5 ms, p99 206 ms, MAX 523 ms over
///     n=20000 under deliberately pathological load (loadavg 118 on 18 cores).
///  2. A PEER'S DEVICE-BARRIER HOLD — a legitimate holder inside the locked
///     section, which spans `sync_all` on the temporary (`atomic_replace_locked`,
///     :1255) and `sync_directory` on the parent (:1271). Both are `F_FULLFSYNC`
///     device-cache barriers on Apple targets, whose latency is bounded by the
///     whole machine's I/O and not by the bytes written. Measured for THIS
///     locked section: 9.13 ms mean / 13.9 ms p99 / 25.5 ms max over n=200 idle,
///     and 20.5 ms mean / 42.9 ms p99 / 69.7 ms max over n=300 at loadavg 17 on
///     18 cores with eight concurrent `fsync` writers. The sibling save path,
///     whose locked section has the same two-barrier shape, was caught at
///     82.5-279.7 ms in six leaked fixtures (`native_document_host.rs:2571`).
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
    /// spelled out at `native_document_host.rs:1411-1422`. Pinned by
    /// `the_event_loop_budget_stays_in_the_sibling_write_lock_class`.
    EventLoop,
    /// The caller is the `aterm-native-document` worker (`app_documents.rs:149`),
    /// whose whole job is to keep exactly this work off the event loop. Parking
    /// it costs no frame and drops no input; refusing costs an append.
    Worker,
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
        }
    }
}

/// The journal's rendering of the crate's ONE bounded advisory-lock retry
/// (`native_document_host::acquire_advisory_lock_within` — same loop, same
/// exponential backoff clamped to the remaining budget, same "retry `WouldBlock`
/// only, never `Error`" rule). This function adds nothing but the product
/// vocabulary and the `path` those messages name.
///
/// Takes `budget` rather than reading a constant so that the value the whole fix
/// turns on is a parameter a test can pin — see
/// `a_permanently_held_journal_lock_is_refused_only_after_the_budget_is_spent`.
fn take_journal_lock(lock: &File, path: &Path, budget: std::time::Duration) -> Result<(), String> {
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
/// It cannot mask the engine fix. The ride-out this change adds is pinned by
/// [`a_journal_lock_held_by_a_peer_is_waited_out_not_reported_busy`] and
/// [`a_permanently_held_journal_lock_is_refused_only_after_the_budget_is_spent`],
/// neither of which goes anywhere near this helper.
///
/// Bounded, so a lock that never frees still fails; and only the busy error is
/// retried — every other error returns on the first attempt.
pub(crate) fn settle_busy<T>(mut attempt: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
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
        // Refuses-without-BLOCKING: a genuine failure parks FOREVER, so any finite
        // bound catches it and only the report latency changes. 1s is the bound this
        // repo has already watched cross under full-suite load (cb8c0cff).
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
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
        // Refuses-without-BLOCKING: a genuine failure parks FOREVER, so any finite
        // bound catches it and only the report latency changes. 1s is the bound this
        // repo has already watched cross under full-suite load (cb8c0cff).
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
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
        // Refuses-without-BLOCKING: a genuine failure parks FOREVER, so any finite
        // bound catches it and only the report latency changes. 1s is the bound this
        // repo has already watched cross under full-suite load (cb8c0cff).
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
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
        // Refuses-without-BLOCKING: a genuine failure parks FOREVER, so any finite
        // bound catches it and only the report latency changes. 1s is the bound this
        // repo has already watched cross under full-suite load (cb8c0cff).
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        assert!(error.contains("busy"), "{error}");
        assert!(error.contains("retry"), "{error}");

        drop(held);
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();
        assert!(recover_journal_for(initialized.key, &fs::read(initialized.path).unwrap()).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    /// A lock a peer still holds is WAITED OUT, not reported as another owner.
    ///
    /// This is the regression the whole fix exists for, and it is pinned WITHOUT a
    /// fork, deliberately. `flock` is per open-file-description, so a second
    /// `open_journal_lock` in THIS process contends with the first in exactly the
    /// way a descriptor a forked child has not `execve`d away yet does — the
    /// sibling module states and relies on the same equivalence
    /// (`native_document_host.rs:2575`). A raw `fork()` from the libtest thread
    /// pool would buy provenance and cost determinism: the parent has to build a
    /// plan between the fork and the append, so a scheduling stall longer than the
    /// child's hold makes the append succeed on its FIRST `try_lock` and the test
    /// pass VACUOUSLY — green against a full revert. It would also inject the very
    /// pathology under test into the suite, since a child that sleeps without
    /// `execve` holds every OTHER thread's descriptor open for its whole life.
    ///
    /// A held lock is realistic at this duration for two independent reasons, both
    /// measured, both named on [`JournalLockPatience`]: a peer inside its two
    /// `F_FULLFSYNC` barriers (82.5-279.7 ms in the sibling's leaked fixtures), and
    /// a child of ours between `fork` and `execve` (max 523 ms at n=20000 under
    /// pathological load, `aterm-pty/src/unix.rs:739-744`).
    ///
    /// DETERMINISM comes from the second assertion, not the first: a regression to
    /// a single `try_lock` returns `Failed` in microseconds, so it fails
    /// `Committed` AND fails `waited >= HELD`. The budget is 2 s against a 200 ms
    /// hold, so no scheduler stall can turn this into a false FAIL either.
    #[test]
    fn a_journal_lock_held_by_a_peer_is_waited_out_not_reported_busy() {
        const HELD: std::time::Duration = std::time::Duration::from_millis(200);

        let root = test_root("waited-lock");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (mut store, id, disk) = snapshots("base");
        let canonical = "file:///tmp/waited-lock.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();

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
        let mut plan = reducer.plan_snapshot(&newer).unwrap();
        plan.expected_image = Some(initialized.image_fingerprint);

        // Stands in for a peer mid-barrier, or for a forked child that has not
        // reached `execve`: a SECOND description onto the same lock file. Taken
        // through the product's own primitive, so this setup cannot be more
        // fragile than the thing it sets up for.
        let lock_path = journal_lock_path(&initialized.path).unwrap();
        let holder = open_journal_lock(&lock_path).unwrap();
        take_journal_lock(
            &holder,
            &initialized.path,
            JournalLockPatience::Worker.budget(),
        )
        .unwrap();
        let released = std::thread::spawn(move || {
            std::thread::sleep(HELD);
            drop(holder);
        });

        let started = std::time::Instant::now();
        let result = execute_journal_append(
            &initialized.path,
            initialized.key,
            &plan,
            JournalLockPatience::Worker,
        );
        let waited = started.elapsed();
        released.join().unwrap();

        assert!(
            matches!(result, JournalAppendResult::Committed(_)),
            "a {HELD:?} holder must be waited out, not reported busy: {result:?}"
        );
        assert!(
            waited >= HELD,
            "the append cannot have published before the holder released: {waited:?}"
        );
        assert_eq!(
            recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap())
                .unwrap()
                .text,
            "base newer"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The budget is SPENT before a refusal, and a holder that never releases is
    /// still refused. Pins the shrink direction: a regression to a single
    /// `try_lock` — or to any budget smaller than the one asked for — returns in
    /// microseconds and fails the `elapsed >= budget` assertion.
    ///
    /// Deliberately NO absolute upper bound: that assertion class is the one
    /// full-suite load has already crossed twice in this crate, and the
    /// refuses-without-blocking contract is owned by
    /// `held_journal_lock_returns_busy_and_a_later_retry_succeeds` and by
    /// [`the_event_loop_budget_stays_in_the_sibling_write_lock_class`], which pins
    /// the growth direction without consulting a clock at all.
    #[test]
    fn a_permanently_held_journal_lock_is_refused_only_after_the_budget_is_spent() {
        let root = test_root("budget-spent");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("base");
        let canonical = "file:///tmp/budget-spent.md";
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let lock_path = journal_lock_path(&decision.path).unwrap();
        let held = open_journal_lock(&lock_path).unwrap();
        held.lock().unwrap();
        let probe = open_journal_lock(&lock_path).unwrap();

        let budget = std::time::Duration::from_millis(30);
        let started = std::time::Instant::now();
        let message = take_journal_lock(&probe, &decision.path, budget).unwrap_err();
        let elapsed = started.elapsed();

        assert!(message.contains("busy"), "{message}");
        assert!(message.contains("retry opening Manual"), "{message}");
        assert!(
            elapsed >= budget,
            "budget must actually be spent, got {elapsed:?}"
        );
        drop(held);
        let _ = fs::remove_dir_all(root);
    }

    /// The value the whole fix turns on, pinned WITHOUT a clock.
    ///
    /// `DraftJournalHost::initialize` runs synchronously on the thread that owns
    /// `App` (`app_documents.rs:2555`), so its budget is a frame budget. Growing it
    /// back toward the 30 s the busy guards assert would freeze the event loop for
    /// exactly that long and every timing assertion in this file would still pass
    /// green — which is why this guard compares constants instead of elapsed time.
    /// Tying it to the sibling write lock's budget is the point: one thread, one
    /// frame argument (`native_document_host.rs:1411-1422`), one number.
    #[test]
    fn the_event_loop_budget_stays_in_the_sibling_write_lock_class() {
        assert_eq!(
            JournalLockPatience::EventLoop.budget(),
            crate::native_document_host::WRITE_LOCK_RETRY_BUDGET,
            "the event-loop journal budget must stay the sibling write lock's budget"
        );
        assert!(
            JournalLockPatience::EventLoop.budget() <= std::time::Duration::from_millis(32),
            "two frames at 60 Hz is the ceiling for anything the event loop waits on"
        );
        assert!(
            JournalLockPatience::Worker.budget() > JournalLockPatience::EventLoop.budget(),
            "the worker is the thread that exists to absorb this wait"
        );
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
        // Refuses-without-BLOCKING: a genuine failure parks FOREVER, so any finite
        // bound catches it and only the report latency changes. 1s is the bound this
        // repo has already watched cross under full-suite load (cb8c0cff).
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
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
        assert_eq!(
            fs::read(preserved.preserved_path.unwrap()).unwrap(),
            original
        );
        drop(other_store);

        fs::write(&preserved.path, b"torn").unwrap();
        let corrupt = host.inspect_open(canonical, b"external").unwrap();
        assert!(matches!(corrupt.notice, Some(RecoveryNotice::Corrupt(_))));
        let kept = settle_busy(|| host.initialize(corrupt.clone(), &external, &external)).unwrap();
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
        let initialized = settle_busy(|| host.initialize(first.clone(), &disk, &disk)).unwrap();
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
        let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();
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

        let test_name = std::thread::current()
            .name()
            .expect("test harness names the current test")
            .to_string();
        let root = test_root("process-race");
        let host = DraftJournalHost::new(root.clone()).unwrap();
        let (_store, _id, disk) = snapshots("base");
        let decision = host.inspect_open(URI, disk.text.as_bytes()).unwrap();
        let initialized = settle_busy(|| host.initialize(decision.clone(), &disk, &disk)).unwrap();
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
}
