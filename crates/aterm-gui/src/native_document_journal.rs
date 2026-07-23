// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable private crash journals for native documents.
//!
//! A journal filename is derived from the canonical URI but never contains it.
//! Every publication uses a same-directory temporary file, `sync_all`, atomic
//! rename, and directory sync. Thus an interrupted append leaves the previous
//! complete image recoverable instead of exposing a valid-prefix ambiguity.

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
}

#[derive(Clone, Debug)]
pub(crate) struct InitializedJournal {
    pub(crate) key: JournalDocumentKey,
    pub(crate) path: PathBuf,
    pub(crate) durable_seq: Seq,
    pub(crate) durable_text: Arc<str>,
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
            let plan = checkpoint_plan(entry.key, generation, &disk, &entry.desired)
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
        let plan = if entry.desired.seq.0 == entry.reducer.durable_seq().0.saturating_add(1) {
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
        match entry.reducer.complete(generation, result) {
            JournalReduction::Durable(checkpoint) => {
                if let Some(text) = entry.append_text.take() {
                    entry.durable_text = text;
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
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(decision),
            Err(error) => return Err(format!("could not read recovery journal: {error}")),
        };
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
        let preserved_path = if decision.preserve_existing && decision.path.exists() {
            Some(self.preserve(&decision.path)?)
        } else {
            None
        };
        let mut records = vec![JournalRecord::snapshot_for(decision.key, disk)];
        if current.seq != disk.seq || current.text != disk.text {
            records.push(JournalRecord::snapshot_for(decision.key, current));
        }
        let bytes = encode_journal(&records).map_err(|error| format!("{error:?}"))?;
        atomic_replace(&decision.path, &bytes).map_err(|error| error.to_string())?;
        Ok(InitializedJournal {
            key: decision.key,
            path: decision.path,
            durable_seq: current.seq,
            durable_text: current.text.clone(),
            notice: decision.notice,
            preserved_path,
        })
    }

    pub(crate) fn path_for(&self, key: JournalDocumentKey) -> PathBuf {
        self.root.join(format!("{:016x}.atdj", key.0))
    }

    fn preserve(&self, path: &Path) -> Result<PathBuf, String> {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("draft");
        for suffix in 1..=10_000_u32 {
            let candidate = self.root.join(format!("{stem}.preserved-{suffix}.atdj"));
            if candidate.exists() {
                continue;
            }
            fs::rename(path, &candidate)
                .map_err(|error| format!("could not preserve recovery journal: {error}"))?;
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
    let result = (|| {
        let existing = fs::read(path).map_err(|error| format!("preflight: {error}"))?;
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
        atomic_replace(path, &image).map_err(|error| error.to_string())?;
        Ok(JournalAppendProof {
            appended_len: plan.bytes.len(),
            encoded_fingerprint: plan.encoded_fingerprint,
            file_synced: true,
            renamed_over_journal: true,
            directory_synced: true,
        })
    })();
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
    match atomic_replace(path, &plan.bytes) {
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

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("journal has no parent directory"))?;
    create_private_dir(parent)?;
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
            Err(error) => return Err(error),
        }
    }
    let (temporary_path, mut temporary_file) = temporary
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AlreadyExists, "temp exhausted"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary_file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    let result = (|| {
        temporary_file.write_all(bytes)?;
        temporary_file.sync_all()?;
        drop(temporary_file);
        replace_file(&temporary_path, path)?;
        sync_directory(parent)
    })();
    if temporary_path.exists() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(temporary, target)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    #[link(name = "kernel32")]
    unsafe extern "system" {
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
        MoveFileExW(
            existing.as_ptr(),
            replacement.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_store::{DocumentStore, TextEdit};

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
}
