// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide native-document truth for Markdown and editor views.
//!
//! One [`Document`] owns one canonical [`aterm_buffer::Surface`].  Views retain only a
//! stable [`DocumentId`] and immutable [`DocumentSnapshot`] projections, so a Markdown
//! preview and an editor can never race private text copies.  All mutations enter through
//! [`DocumentStore::transact`], are OCC-guarded by [`Seq`], and publish exactly one new
//! sequence for an atomic multi-selection edit.

#![allow(
    dead_code,
    reason = "native tab-app integration lands in staged consumers"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::ops::Range;
use std::sync::Arc;

use aterm_buffer::{Edit as SurfaceEdit, LineId, Seq, Surface, SurfaceId, TxnOutcome, WriteCap};

/// Stable process-local identity shared by every presentation of one canonical document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DocumentId(NonZeroU64);

impl DocumentId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

/// A view reference is deliberately independent of app kind: Markdown and Editor views
/// participate in the same last-reference close decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct DocumentViewId(pub(crate) u64);

/// File identity observed when the projection was loaded/saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FileVersion {
    pub(crate) content_fingerprint: u64,
}

/// Immutable read projection. `text` is derived from the canonical Surface at `seq` and
/// shared by every reader until the next commit.
#[derive(Clone, Debug)]
pub(crate) struct DocumentSnapshot {
    pub(crate) id: DocumentId,
    pub(crate) seq: Seq,
    pub(crate) file_version: FileVersion,
    pub(crate) text: Arc<str>,
}

/// One half-open UTF-8 byte edit. A transaction may contain multiple non-overlapping
/// edits; they commit as one Surface event/sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextEdit {
    pub(crate) range: Range<usize>,
    pub(crate) insert: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DocumentTxnOutcome {
    Committed {
        seq: Seq,
        snapshot: DocumentSnapshotMeta,
        deltas: Vec<EditDelta>,
    },
    Conflict {
        current: Seq,
    },
    Rejected(DocumentError),
}

/// Copy-small metadata returned on the mutation path. Consumers pull the shared snapshot
/// from the store rather than receiving copied text in the completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DocumentSnapshotMeta {
    pub(crate) id: DocumentId,
    pub(crate) seq: Seq,
    pub(crate) revision: u64,
}

/// A committed byte-range transform used to rebase per-view selections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditDelta {
    pub(crate) old: Range<usize>,
    pub(crate) inserted_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DocumentError {
    UnknownDocument,
    DuplicateCanonicalUri,
    UnknownView,
    Closing,
    InvalidRange,
    OverlappingEdits,
    CloseNotReady,
    CheckpointAheadOfHead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocumentPhase {
    Active,
    Closing { requested: Seq },
    Blocked { requested: Seq },
    Suspended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocumentCloseReadiness {
    Ready { requested: Seq },
    Pending { requested: Seq },
    Blocked { requested: Seq },
}

#[derive(Clone, Debug)]
struct Document {
    id: DocumentId,
    canonical_uri: String,
    surface: Surface,
    text: crate::native_text::TextRope,
    /// Derived cache only; Surface is the mutation authority.
    projection: Arc<str>,
    revision: u64,
    file_version: FileVersion,
    checkpoint_seq: Seq,
    /// Every attached controller's published document sequence. Publication happens in
    /// the same synchronous mutation lane as the canonical Surface commit, before the
    /// caller can route another input event.
    views: BTreeMap<DocumentViewId, Seq>,
    phase: DocumentPhase,
}

impl Document {
    fn head(&self) -> Seq {
        self.surface.seq()
    }

    fn dirty(&self) -> bool {
        self.checkpoint_seq < self.head()
    }

    fn snapshot(&self) -> DocumentSnapshot {
        DocumentSnapshot {
            id: self.id,
            seq: self.head(),
            file_version: self.file_version,
            text: self.projection.clone(),
        }
    }
}

/// Serialized owner of native documents. It performs no filesystem I/O; read/write grants
/// and atomic persistence are host effects layered above this deterministic core.
#[derive(Default)]
pub(crate) struct DocumentStore {
    next_id: u64,
    documents: BTreeMap<DocumentId, Document>,
    by_uri: BTreeMap<String, DocumentId>,
}

impl DocumentStore {
    pub(crate) fn new() -> Self {
        Self {
            next_id: 1,
            documents: BTreeMap::new(),
            by_uri: BTreeMap::new(),
        }
    }

    /// Open or reuse the unique document for a canonical URI.
    pub(crate) fn open(&mut self, canonical_uri: String, text: String) -> DocumentId {
        if let Some(id) = self.by_uri.get(&canonical_uri) {
            return *id;
        }
        let raw = self.next_id.max(1);
        self.next_id = raw.saturating_add(1);
        let nonzero = NonZeroU64::new(raw).expect("document ids start at one");
        let id = DocumentId(nonzero);
        let mut surface = Surface::new(SurfaceId(nonzero));
        // Surface owns the original String allocation; the flat Arc projection
        // is the separate immutable cache shared by every document snapshot.
        let projection: Arc<str> = Arc::from(text.as_str());
        surface.apply(&WriteCap, SurfaceEdit::AppendLine(text));
        let head = surface.seq();
        let text_rope = crate::native_text::TextRope::from(projection.as_ref());
        let content_fingerprint = fingerprint(&projection);
        let document = Document {
            id,
            canonical_uri: canonical_uri.clone(),
            surface,
            text: text_rope,
            projection,
            revision: 1,
            file_version: FileVersion {
                content_fingerprint,
            },
            checkpoint_seq: head,
            views: BTreeMap::new(),
            phase: DocumentPhase::Suspended,
        };
        self.by_uri.insert(canonical_uri, id);
        self.documents.insert(id, document);
        id
    }

    pub(crate) fn id_for_uri(&self, canonical_uri: &str) -> Option<DocumentId> {
        self.by_uri.get(canonical_uri).copied()
    }

    pub(crate) fn canonical_uri(&self, id: DocumentId) -> Option<&str> {
        self.documents.get(&id).map(|d| d.canonical_uri.as_str())
    }

    /// Roll back a failed host initialization before any controller observes the
    /// document. Once a view is attached, normal close/durability rules own
    /// removal and this seam fails closed.
    pub(crate) fn remove_if_unattached(&mut self, id: DocumentId) -> Result<(), DocumentError> {
        let document = self
            .documents
            .get(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        if !document.views.is_empty() {
            return Err(DocumentError::CloseNotReady);
        }
        let uri = document.canonical_uri.clone();
        self.documents.remove(&id);
        self.by_uri.remove(&uri);
        Ok(())
    }

    pub(crate) fn snapshot(&self, id: DocumentId) -> Option<DocumentSnapshot> {
        self.documents.get(&id).map(Document::snapshot)
    }

    pub(crate) fn revision(&self, id: DocumentId) -> Option<u64> {
        self.documents.get(&id).map(|d| d.revision)
    }

    pub(crate) fn dirty(&self, id: DocumentId) -> Option<bool> {
        self.documents.get(&id).map(Document::dirty)
    }

    pub(crate) fn checkpoint_seq(&self, id: DocumentId) -> Option<Seq> {
        self.documents
            .get(&id)
            .map(|document| document.checkpoint_seq)
    }

    pub(crate) fn phase(&self, id: DocumentId) -> Option<DocumentPhase> {
        self.documents.get(&id).map(|d| d.phase)
    }

    pub(crate) fn attach_view(
        &mut self,
        id: DocumentId,
        view: DocumentViewId,
    ) -> Result<(), DocumentError> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        let observed = document.head();
        document.views.insert(view, observed);
        document.phase = DocumentPhase::Active;
        Ok(())
    }

    pub(crate) fn view_count(&self, id: DocumentId) -> Option<usize> {
        self.documents.get(&id).map(|d| d.views.len())
    }

    /// Stable document identities currently owned by the process.  Shutdown uses
    /// this store-owned enumeration instead of reconstructing ownership from the
    /// visible tab tree: even a suspended document must participate in a clean-quit
    /// durability barrier.
    pub(crate) fn document_ids(&self) -> Vec<DocumentId> {
        self.documents.keys().copied().collect()
    }

    /// Every view reference recorded by the canonical document owner.
    pub(crate) fn view_ids(&self, id: DocumentId) -> Option<Vec<DocumentViewId>> {
        self.documents
            .get(&id)
            .map(|document| document.views.keys().copied().collect())
    }

    pub(crate) fn observed_seq(&self, id: DocumentId, view: DocumentViewId) -> Option<Seq> {
        self.documents
            .get(&id)
            .and_then(|document| document.views.get(&view))
            .copied()
    }

    /// Apply one atomic multi-edit at `base`. Edits must be UTF-8-boundary-valid and
    /// non-overlapping in the pre-transaction snapshot. All changes become one Surface
    /// transaction event, so rapid callers can only observe whole revisions.
    pub(crate) fn transact(
        &mut self,
        id: DocumentId,
        base: Seq,
        mut edits: Vec<TextEdit>,
    ) -> DocumentTxnOutcome {
        let Some(document) = self.documents.get_mut(&id) else {
            return DocumentTxnOutcome::Rejected(DocumentError::UnknownDocument);
        };
        if !matches!(
            document.phase,
            DocumentPhase::Active | DocumentPhase::Suspended
        ) {
            return DocumentTxnOutcome::Rejected(DocumentError::Closing);
        }
        let head = document.head();
        if head != base {
            return DocumentTxnOutcome::Conflict { current: head };
        }
        if edits.is_empty() {
            return DocumentTxnOutcome::Committed {
                seq: head,
                snapshot: DocumentSnapshotMeta {
                    id,
                    seq: head,
                    revision: document.revision,
                },
                deltas: Vec::new(),
            };
        }
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let source = document.projection.as_ref();
        for edit in &edits {
            if edit.range.start > edit.range.end
                || edit.range.end > source.len()
                || !source.is_char_boundary(edit.range.start)
                || !source.is_char_boundary(edit.range.end)
            {
                return DocumentTxnOutcome::Rejected(DocumentError::InvalidRange);
            }
        }
        for pair in edits.windows(2) {
            if pair[0].range.end > pair[1].range.start {
                return DocumentTxnOutcome::Rejected(DocumentError::OverlappingEdits);
            }
        }

        let rope_edits = edits
            .iter()
            .map(|edit| (edit.range.clone(), edit.insert.as_str()))
            .collect::<Vec<_>>();
        let Ok(next_text) = document.text.replace_many(&rope_edits) else {
            return DocumentTxnOutcome::Rejected(DocumentError::InvalidRange);
        };
        // Flatten once for Surface, then publish a separate immutable projection
        // allocation shared by every read snapshot. Keeping the frozen Surface
        // Edit(String) API costs this one copy at the ownership boundary.
        let next = next_text.to_flat_string();
        let next_projection: Arc<str> = Arc::from(next.as_str());
        let deltas = edits
            .iter()
            .map(|edit| EditDelta {
                old: edit.range.clone(),
                inserted_len: edit.insert.len(),
            })
            .collect::<Vec<_>>();
        let outcome =
            document
                .surface
                .transact(&WriteCap, base, vec![SurfaceEdit::SetLine(LineId(0), next)]);
        let Seq(seq) = match outcome {
            TxnOutcome::Committed(seq) => seq,
            TxnOutcome::Conflict => {
                return DocumentTxnOutcome::Conflict {
                    current: document.head(),
                };
            }
        };
        let committed = Seq(seq);
        document.text = next_text;
        document.projection = next_projection;
        document.revision = document.revision.saturating_add(1);
        document.file_version = FileVersion {
            content_fingerprint: fingerprint(&document.projection),
        };
        for observed in document.views.values_mut() {
            *observed = committed;
        }
        DocumentTxnOutcome::Committed {
            seq: committed,
            snapshot: DocumentSnapshotMeta {
                id,
                seq: committed,
                revision: document.revision,
            },
            deltas,
        }
    }

    /// Prepare removal of `closing`. If those are the final live views, capture a
    /// mandatory requested sequence and require durability before detach.
    pub(crate) fn prepare_close(
        &mut self,
        id: DocumentId,
        closing: &[DocumentViewId],
    ) -> Result<DocumentCloseReadiness, DocumentError> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        if closing
            .iter()
            .any(|view| !document.views.contains_key(view))
        {
            return Err(DocumentError::UnknownView);
        }
        let closing_set = closing.iter().copied().collect::<BTreeSet<_>>();
        let remaining = document
            .views
            .keys()
            .filter(|view| !closing_set.contains(view))
            .count();
        let requested = document.head();
        if remaining > 0 || document.checkpoint_seq >= requested {
            return Ok(DocumentCloseReadiness::Ready { requested });
        }
        document.phase = match document.phase {
            DocumentPhase::Blocked { .. } => DocumentPhase::Blocked { requested },
            _ => DocumentPhase::Closing { requested },
        };
        Ok(match document.phase {
            DocumentPhase::Blocked { requested } => DocumentCloseReadiness::Blocked { requested },
            _ => DocumentCloseReadiness::Pending { requested },
        })
    }

    pub(crate) fn checkpoint_ack(
        &mut self,
        id: DocumentId,
        durable: Seq,
    ) -> Result<DocumentCloseReadiness, DocumentError> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        if durable > document.head() {
            return Err(DocumentError::CheckpointAheadOfHead);
        }
        if durable > document.checkpoint_seq {
            document.checkpoint_seq = durable;
        }
        let requested = match document.phase {
            DocumentPhase::Closing { requested } | DocumentPhase::Blocked { requested } => {
                requested
            }
            DocumentPhase::Active | DocumentPhase::Suspended => document.head(),
        };
        if document.checkpoint_seq >= requested {
            Ok(DocumentCloseReadiness::Ready { requested })
        } else {
            Ok(DocumentCloseReadiness::Pending { requested })
        }
    }

    pub(crate) fn checkpoint_fail(&mut self, id: DocumentId) -> Result<Seq, DocumentError> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        let requested = match document.phase {
            DocumentPhase::Closing { requested } | DocumentPhase::Blocked { requested } => {
                requested
            }
            DocumentPhase::Active | DocumentPhase::Suspended => document.head(),
        };
        document.phase = DocumentPhase::Blocked { requested };
        Ok(requested)
    }

    pub(crate) fn checkpoint_retry(
        &mut self,
        id: DocumentId,
    ) -> Result<DocumentCloseReadiness, DocumentError> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        let requested = document.head();
        if document.checkpoint_seq >= requested {
            return Ok(DocumentCloseReadiness::Ready { requested });
        }
        document.phase = DocumentPhase::Closing { requested };
        Ok(DocumentCloseReadiness::Pending { requested })
    }

    pub(crate) fn abort_close(&mut self, id: DocumentId) -> Result<(), DocumentError> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        document.phase = if document.views.is_empty() {
            DocumentPhase::Suspended
        } else {
            DocumentPhase::Active
        };
        Ok(())
    }

    /// Commit the already-prepared view detach. A final dirty document cannot cross this
    /// seam until its mandatory requested sequence is durable.
    pub(crate) fn commit_detach(
        &mut self,
        id: DocumentId,
        closing: &[DocumentViewId],
    ) -> Result<(), DocumentError> {
        match self.prepare_close(id, closing)? {
            DocumentCloseReadiness::Ready { .. } => {}
            DocumentCloseReadiness::Pending { .. } | DocumentCloseReadiness::Blocked { .. } => {
                return Err(DocumentError::CloseNotReady);
            }
        }
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        for view in closing {
            document.views.remove(view);
        }
        document.phase = if document.views.is_empty() {
            DocumentPhase::Suspended
        } else {
            DocumentPhase::Active
        };
        Ok(())
    }

    /// Idempotent central teardown seam for one view edge.  A caller that reaches
    /// this without first satisfying the final-dirty durability barrier fails
    /// closed: the canonical reference remains installed and `CloseNotReady` is
    /// returned.  Tab/window teardown calls this even after a batch detach, hence
    /// an already-absent view is a successful no-op.
    pub(crate) fn detach_view_if_ready(
        &mut self,
        id: DocumentId,
        view: DocumentViewId,
    ) -> Result<bool, DocumentError> {
        let document = self
            .documents
            .get(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        if !document.views.contains_key(&view) {
            return Ok(false);
        }
        self.commit_detach(id, &[view])?;
        Ok(true)
    }

    /// Atomically detach all document leaves in one already-prepared close plan.
    /// The first pass obtains a Ready verdict for *every* document without
    /// removing anything; only then does the infallible mutation pass run.  This
    /// is the concrete multi-document form of `NativeClosePlan`'s
    /// `document_ready / other_leaf_ready` gate.
    pub(crate) fn commit_detach_batch(
        &mut self,
        closing: &[(DocumentId, Vec<DocumentViewId>)],
    ) -> Result<(), DocumentError> {
        for (document, views) in closing {
            match self.prepare_close(*document, views)? {
                DocumentCloseReadiness::Ready { .. } => {}
                DocumentCloseReadiness::Pending { .. } | DocumentCloseReadiness::Blocked { .. } => {
                    return Err(DocumentError::CloseNotReady);
                }
            }
        }

        // Every key/view was validated above and no intervening mutation is
        // possible through `&mut self`, so this second pass cannot partially fail.
        for (document, views) in closing {
            let state = self
                .documents
                .get_mut(document)
                .expect("batch preflight retained every document");
            for view in views {
                let removed = state.views.remove(view);
                debug_assert!(removed.is_some(), "batch preflight retained every view");
            }
            state.phase = if state.views.is_empty() {
                DocumentPhase::Suspended
            } else {
                DocumentPhase::Active
            };
        }
        Ok(())
    }

    /// Read canonical text back through the Surface for conformance tests. Production
    /// views use the cached immutable projection.
    #[cfg(test)]
    fn surface_text(&self, id: DocumentId) -> Option<String> {
        let document = self.documents.get(&id)?;
        let read = document.surface.read_text(
            &aterm_buffer::ReadCap,
            aterm_buffer::Range {
                start: LineId(0),
                end: LineId(1),
            },
        );
        read.text.strip_suffix('\n').map(str::to_owned)
    }

    #[cfg(test)]
    fn rope_metrics(&self, id: DocumentId) -> Option<(usize, u8, usize)> {
        self.documents.get(&id).map(|document| {
            (
                document.text.chunk_count(),
                document.text.height(),
                document.text.line_count(),
            )
        })
    }
}

fn fingerprint(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Rebase one byte position through committed edits. Positions inside a replaced range
/// resolve to the end of inserted content; positions after it shift by the byte delta.
pub(crate) fn rebase_position(mut position: usize, deltas: &[EditDelta]) -> usize {
    let original = position;
    let mut growth = 0isize;
    for delta in deltas {
        if original < delta.old.start {
            break;
        }
        if original <= delta.old.end {
            return delta
                .old
                .start
                .saturating_add_signed(growth)
                .saturating_add(delta.inserted_len);
        }
        let removed = delta.old.end.saturating_sub(delta.old.start);
        growth = growth.saturating_add(delta.inserted_len as isize - removed as isize);
    }
    position = original.saturating_add_signed(growth);
    position
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> (DocumentStore, DocumentId) {
        let mut store = DocumentStore::new();
        let id = store.open("mem://readme".into(), "hello\nworld".into());
        (store, id)
    }

    #[test]
    fn canonical_uri_reuses_one_document() {
        let (mut store, id) = open();
        let again = store.open("mem://readme".into(), "ignored".into());
        assert_eq!(again, id);
        assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "hello\nworld");
    }

    #[test]
    fn large_document_uses_balanced_chunk_storage_behind_surface_projection() {
        let mut store = DocumentStore::new();
        let source = (0..3_000)
            .map(|line| format!("line {line} — persistent text\n"))
            .collect::<String>();
        let id = store.open("mem://large".into(), source.clone());
        let (chunks, height, lines) = store.rope_metrics(id).unwrap();
        assert!(chunks > 2);
        assert!(usize::from(height) <= chunks.ilog2() as usize + 2);
        assert_eq!(lines, 3_001);
        assert_eq!(store.snapshot(id).unwrap().text.as_ref(), source);
        assert_eq!(store.surface_text(id).as_deref(), Some(source.as_str()));
    }

    #[test]
    fn multi_edit_commits_one_seq_and_surface_matches_projection() {
        let (mut store, id) = open();
        let base = store.snapshot(id).unwrap().seq;
        let outcome = store.transact(
            id,
            base,
            vec![
                TextEdit {
                    range: 0..5,
                    insert: "goodbye".into(),
                },
                TextEdit {
                    range: 6..11,
                    insert: "moon".into(),
                },
            ],
        );
        let DocumentTxnOutcome::Committed { seq, .. } = outcome else {
            panic!("expected commit");
        };
        assert_eq!(seq.0, base.0 + 1, "one transaction is one spine event");
        assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "goodbye\nmoon");
        assert_eq!(store.surface_text(id).as_deref(), Some("goodbye\nmoon"));
    }

    #[test]
    fn snapshots_share_the_committed_projection_allocation() {
        let (mut store, id) = open();
        let before = store.snapshot(id).unwrap();
        let DocumentTxnOutcome::Committed { .. } = store.transact(
            id,
            before.seq,
            vec![TextEdit {
                range: 5..5,
                insert: "!".into(),
            }],
        ) else {
            panic!("expected commit");
        };

        let projection = &store.documents.get(&id).unwrap().projection;
        let first = store.snapshot(id).unwrap();
        let second = store.snapshot(id).unwrap();
        assert!(Arc::ptr_eq(projection, &first.text));
        assert!(Arc::ptr_eq(&first.text, &second.text));
        assert_eq!(first.text.as_ref(), "hello!\nworld");
    }

    #[test]
    fn commit_publishes_the_same_seq_to_every_attached_controller() {
        let (mut store, id) = open();
        let markdown = DocumentViewId(11);
        let editor = DocumentViewId(12);
        store.attach_view(id, markdown).unwrap();
        store.attach_view(id, editor).unwrap();
        let base = store.snapshot(id).unwrap().seq;
        let DocumentTxnOutcome::Committed { seq, .. } = store.transact(
            id,
            base,
            vec![TextEdit {
                range: 5..5,
                insert: "!".into(),
            }],
        ) else {
            panic!("expected commit");
        };
        assert_eq!(store.observed_seq(id, markdown), Some(seq));
        assert_eq!(store.observed_seq(id, editor), Some(seq));
        assert_eq!(store.snapshot(id).unwrap().seq, seq);
    }

    #[test]
    fn stale_or_invalid_transactions_change_nothing() {
        let (mut store, id) = open();
        let base = store.snapshot(id).unwrap().seq;
        let first = store.transact(
            id,
            base,
            vec![TextEdit {
                range: 0..5,
                insert: "hi".into(),
            }],
        );
        assert!(matches!(first, DocumentTxnOutcome::Committed { .. }));
        let before = store.snapshot(id).unwrap();
        assert_eq!(
            store.transact(
                id,
                base,
                vec![TextEdit {
                    range: 0..2,
                    insert: "no".into(),
                }]
            ),
            DocumentTxnOutcome::Conflict {
                current: before.seq
            }
        );
        assert!(matches!(
            store.transact(
                id,
                before.seq,
                vec![TextEdit {
                    range: 1..99,
                    insert: String::new(),
                }]
            ),
            DocumentTxnOutcome::Rejected(DocumentError::InvalidRange)
        ));
        assert_eq!(store.snapshot(id).unwrap().text, before.text);
    }

    #[test]
    fn final_view_of_dirty_shared_document_waits_for_durable_ack() {
        let (mut store, id) = open();
        let markdown = DocumentViewId(1);
        let editor = DocumentViewId(2);
        store.attach_view(id, markdown).unwrap();
        store.attach_view(id, editor).unwrap();
        let base = store.snapshot(id).unwrap().seq;
        let DocumentTxnOutcome::Committed { seq, .. } = store.transact(
            id,
            base,
            vec![TextEdit {
                range: 5..5,
                insert: "!".into(),
            }],
        ) else {
            panic!("commit");
        };

        assert!(matches!(
            store.prepare_close(id, &[editor]).unwrap(),
            DocumentCloseReadiness::Ready { .. }
        ));
        store.commit_detach(id, &[editor]).unwrap();
        assert!(matches!(
            store.prepare_close(id, &[markdown]).unwrap(),
            DocumentCloseReadiness::Pending { requested } if requested == seq
        ));
        assert_eq!(
            store.commit_detach(id, &[markdown]),
            Err(DocumentError::CloseNotReady)
        );
        assert!(matches!(
            store.checkpoint_ack(id, seq).unwrap(),
            DocumentCloseReadiness::Ready { .. }
        ));
        store.commit_detach(id, &[markdown]).unwrap();
        assert_eq!(store.view_count(id), Some(0));
    }

    #[test]
    fn failed_checkpoint_preserves_views_until_retry() {
        let (mut store, id) = open();
        let view = DocumentViewId(7);
        store.attach_view(id, view).unwrap();
        let base = store.snapshot(id).unwrap().seq;
        let DocumentTxnOutcome::Committed { seq, .. } = store.transact(
            id,
            base,
            vec![TextEdit {
                range: 0..0,
                insert: "x".into(),
            }],
        ) else {
            panic!("commit");
        };
        store.prepare_close(id, &[view]).unwrap();
        store.checkpoint_fail(id).unwrap();
        assert!(matches!(
            store.prepare_close(id, &[view]).unwrap(),
            DocumentCloseReadiness::Blocked { .. }
        ));
        assert_eq!(store.view_count(id), Some(1));
        assert!(matches!(
            store.checkpoint_retry(id).unwrap(),
            DocumentCloseReadiness::Pending { .. }
        ));
        store.checkpoint_ack(id, seq).unwrap();
        store.commit_detach(id, &[view]).unwrap();
    }

    #[test]
    fn batch_detach_is_atomic_across_documents() {
        let mut store = DocumentStore::new();
        let first = store.open("mem://batch/first".into(), "one".into());
        let second = store.open("mem://batch/second".into(), "two".into());
        let first_view = DocumentViewId(31);
        let second_view = DocumentViewId(32);
        store.attach_view(first, first_view).unwrap();
        store.attach_view(second, second_view).unwrap();
        let first_seq = match store.transact(
            first,
            store.snapshot(first).unwrap().seq,
            vec![TextEdit {
                range: 3..3,
                insert: "!".into(),
            }],
        ) {
            DocumentTxnOutcome::Committed { seq, .. } => seq,
            other => panic!("first edit failed: {other:?}"),
        };
        let second_seq = match store.transact(
            second,
            store.snapshot(second).unwrap().seq,
            vec![TextEdit {
                range: 3..3,
                insert: "!".into(),
            }],
        ) {
            DocumentTxnOutcome::Committed { seq, .. } => seq,
            other => panic!("second edit failed: {other:?}"),
        };
        store.prepare_close(first, &[first_view]).unwrap();
        store.prepare_close(second, &[second_view]).unwrap();
        store.checkpoint_ack(first, first_seq).unwrap();

        let batch = vec![(first, vec![first_view]), (second, vec![second_view])];
        assert_eq!(
            store.commit_detach_batch(&batch),
            Err(DocumentError::CloseNotReady)
        );
        assert_eq!(store.view_count(first), Some(1));
        assert_eq!(store.view_count(second), Some(1));

        store.checkpoint_ack(second, second_seq).unwrap();
        store.commit_detach_batch(&batch).unwrap();
        assert_eq!(store.view_count(first), Some(0));
        assert_eq!(store.view_count(second), Some(0));
    }

    #[test]
    fn central_view_detach_is_idempotent_and_fails_closed_when_dirty() {
        let (mut store, id) = open();
        let view = DocumentViewId(41);
        store.attach_view(id, view).unwrap();
        let seq = match store.transact(
            id,
            store.snapshot(id).unwrap().seq,
            vec![TextEdit {
                range: 0..0,
                insert: "dirty".into(),
            }],
        ) {
            DocumentTxnOutcome::Committed { seq, .. } => seq,
            other => panic!("edit failed: {other:?}"),
        };
        assert_eq!(
            store.detach_view_if_ready(id, view),
            Err(DocumentError::CloseNotReady)
        );
        assert_eq!(store.view_count(id), Some(1));
        store.checkpoint_ack(id, seq).unwrap();
        assert_eq!(store.detach_view_if_ready(id, view), Ok(true));
        assert_eq!(store.detach_view_if_ready(id, view), Ok(false));
    }

    #[test]
    fn rebase_positions_tracks_atomic_deltas() {
        let deltas = vec![
            EditDelta {
                old: 2..4,
                inserted_len: 5,
            },
            EditDelta {
                old: 8..9,
                inserted_len: 0,
            },
        ];
        assert_eq!(rebase_position(1, &deltas), 1);
        assert_eq!(rebase_position(3, &deltas), 7);
        assert_eq!(rebase_position(10, &deltas), 12);

        let coordinates_stay_in_the_pre_transaction_snapshot = vec![
            EditDelta {
                old: 0..0,
                inserted_len: 10,
            },
            EditDelta {
                old: 10..12,
                inserted_len: 0,
            },
        ];
        assert_eq!(
            rebase_position(6, &coordinates_stay_in_the_pre_transaction_snapshot),
            16
        );
    }
}
