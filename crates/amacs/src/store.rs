// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide native-document truth for Markdown and editor views.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::ops::Range;
use std::sync::Arc;

use aterm_buffer::{Edit as SurfaceEdit, LineId, Seq, Surface, TxnOutcome, WriteCap};

/// Stable process-local identity shared by every presentation of one canonical document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentId(NonZeroU64);

impl DocumentId {
    #[inline]
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A view reference is deliberately independent of app kind: Markdown and Editor views participate in the same last-reference close decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentViewId(pub u64);

/// Immutable read projection.
#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub id: DocumentId,
    pub seq: Seq,
    pub text: Arc<str>,
}

/// One half-open UTF-8 byte edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub insert: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentTxnOutcome {
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

/// Copy-small metadata returned on the mutation path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentSnapshotMeta {
    pub id: DocumentId,
    pub seq: Seq,
    pub revision: u64,
}

/// A committed byte-range transform used to rebase per-view selections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditDelta {
    pub old: Range<usize>,
    pub inserted_len: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentError {
    UnknownDocument,
    UnknownView,
    Closing,
    InvalidRange,
    OverlappingEdits,
    CloseNotReady,
    CheckpointAheadOfHead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentPhase {
    Active,
    Closing { requested: Seq },
    Blocked { requested: Seq },
    Suspended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentCloseReadiness {
    Ready { requested: Seq },
    Pending { requested: Seq },
    Blocked { requested: Seq },
}

#[derive(Clone, Debug)]
struct Document {
    id: DocumentId,
    canonical_uri: String,
    surface: Surface,
    text: crate::text::TextRope,
    /// Derived cache only; Surface is the mutation authority.
    projection: Arc<str>,
    revision: u64,
    checkpoint_seq: Seq,
    /// A failed checkpoint remains evidence until a durable acknowledgement covers its sequence.
    failed_checkpoint: Option<Seq>,
    /// Each attached view's published sequence, updated in the same synchronous lane as the Surface commit, before another input event can be routed.
    views: BTreeMap<DocumentViewId, Seq>,
    phase: DocumentPhase,
}

impl Document {
    #[inline]
    fn head(&self) -> Seq {
        self.surface.seq()
    }

    #[inline]
    fn dirty(&self) -> bool {
        self.checkpoint_seq < self.head()
    }

    #[inline]
    fn snapshot(&self) -> DocumentSnapshot {
        DocumentSnapshot {
            id: self.id,
            seq: self.head(),
            text: self.projection.clone(),
        }
    }
}

/// Serialized owner of native documents.
pub struct DocumentStore {
    next_id: u64,
    documents: BTreeMap<DocumentId, Document>,
    by_uri: BTreeMap<String, DocumentId>,
    /// The buffer-write cap every Surface edit presents, granted in `warm_prologue`.
    write: WriteCap,
}

impl DocumentStore {
    pub const fn new(write: WriteCap) -> Self {
        Self {
            next_id: 1,
            documents: BTreeMap::new(),
            by_uri: BTreeMap::new(),
            write,
        }
    }

    /// A store whose write cap comes from a test-local authority.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::new(test_authority().grant(aterm_cap::Tier::Trusted))
    }

    /// Open or reuse the unique document for a canonical URI.
    pub fn open(&mut self, canonical_uri: String, text: String) -> DocumentId {
        if let Some(id) = self.by_uri.get(&canonical_uri) {
            return *id;
        }
        let raw = self.next_id.max(1);
        self.next_id = raw.saturating_add(1);
        let nonzero = NonZeroU64::new(raw).expect("document ids start at one");
        let id = DocumentId(nonzero);
        let mut surface = Surface::new();
        let projection: Arc<str> = Arc::from(text.as_str());
        surface.apply(&self.write, SurfaceEdit::AppendLine(text));
        let head = surface.seq();
        let text_rope = crate::text::TextRope::from(projection.as_ref());
        let document = Document {
            id,
            canonical_uri: canonical_uri.clone(),
            surface,
            text: text_rope,
            projection,
            revision: 1,
            checkpoint_seq: head,
            failed_checkpoint: None,
            views: BTreeMap::new(),
            phase: DocumentPhase::Suspended,
        };
        self.by_uri.insert(canonical_uri, id);
        self.documents.insert(id, document);
        id
    }

    pub fn id_for_uri(&self, canonical_uri: &str) -> Option<DocumentId> {
        self.by_uri.get(canonical_uri).copied()
    }

    pub fn canonical_uri(&self, id: DocumentId) -> Option<&str> {
        self.documents.get(&id).map(|d| d.canonical_uri.as_str())
    }

    /// Roll back a failed host initialization before any view attaches.
    pub fn remove_if_unattached(&mut self, id: DocumentId) -> Result<(), DocumentError> {
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

    /// Let go of a Suspended document whose text is the file's: no view, and every edit checkpointed.
    pub fn release(&mut self, id: DocumentId) -> Result<(), DocumentError> {
        let document = self
            .documents
            .get(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        if !document.views.is_empty()
            || document.phase != DocumentPhase::Suspended
            || document.dirty()
        {
            return Err(DocumentError::CloseNotReady);
        }
        self.remove_if_unattached(id)
    }

    pub fn snapshot(&self, id: DocumentId) -> Option<DocumentSnapshot> {
        self.documents.get(&id).map(Document::snapshot)
    }

    pub fn revision(&self, id: DocumentId) -> Option<u64> {
        self.documents.get(&id).map(|d| d.revision)
    }

    pub fn dirty(&self, id: DocumentId) -> Option<bool> {
        self.documents.get(&id).map(Document::dirty)
    }

    pub fn checkpoint_seq(&self, id: DocumentId) -> Option<Seq> {
        self.documents
            .get(&id)
            .map(|document| document.checkpoint_seq)
    }

    /// The failed checkpoint no later durable acknowledgement has covered, independent of close state.
    pub fn failed_checkpoint_seq(&self, id: DocumentId) -> Option<Seq> {
        self.documents
            .get(&id)
            .and_then(|document| document.failed_checkpoint)
    }

    pub fn phase(&self, id: DocumentId) -> Option<DocumentPhase> {
        self.documents.get(&id).map(|d| d.phase)
    }

    pub fn attach_view(
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

    #[cfg(any(test, debug_assertions))]
    pub fn view_count(&self, id: DocumentId) -> Option<usize> {
        self.documents.get(&id).map(|d| d.views.len())
    }

    /// Every document the process owns.
    pub fn document_ids(&self) -> Vec<DocumentId> {
        self.documents.keys().copied().collect()
    }

    /// Every view reference recorded by the canonical document owner.
    pub fn view_ids(&self, id: DocumentId) -> Option<Vec<DocumentViewId>> {
        self.documents
            .get(&id)
            .map(|document| document.views.keys().copied().collect())
    }

    #[cfg(any(test, debug_assertions))]
    pub fn observed_seq(&self, id: DocumentId, view: DocumentViewId) -> Option<Seq> {
        self.documents
            .get(&id)
            .and_then(|document| document.views.get(&view))
            .copied()
    }

    /// Apply one atomic multi-edit at `base`.
    pub fn transact(
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
        let next = next_text.to_flat_string();
        let next_projection: Arc<str> = Arc::from(next.as_str());
        let deltas = edits
            .iter()
            .map(|edit| EditDelta {
                old: edit.range.clone(),
                inserted_len: edit.insert.len(),
            })
            .collect::<Vec<_>>();
        let outcome = document.surface.transact(
            &self.write,
            base,
            vec![SurfaceEdit::SetLine(LineId(0), next)],
        );
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

    /// Make the document's text `text` at `base`, as one atomic commit of only
    /// the spans that differ ([`crate::diff::minimal_edits`]): a selection,
    /// mark or scroll anchor rebased through the deltas stays on its text
    /// wherever that text is unchanged, where one `0..len` edit would send
    /// every one of them to the end.
    pub fn replace_text(&mut self, id: DocumentId, base: Seq, text: &str) -> DocumentTxnOutcome {
        let edits = match self.documents.get(&id) {
            Some(document) => crate::diff::minimal_edits(document.projection.as_ref(), text),
            None => return DocumentTxnOutcome::Rejected(DocumentError::UnknownDocument),
        };
        self.transact(id, base, edits)
    }

    /// Prepare removal of `closing`.
    pub fn prepare_close(
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

    pub fn checkpoint_ack(
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
        if document
            .failed_checkpoint
            .is_some_and(|failed| durable >= failed)
        {
            document.failed_checkpoint = None;
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

    /// Record a checkpoint failure without starting a close; an existing close keeps its frozen sequence.
    pub fn checkpoint_fail(&mut self, id: DocumentId) -> Result<Seq, DocumentError> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        let requested = match document.phase {
            DocumentPhase::Closing { requested } | DocumentPhase::Blocked { requested } => {
                document.phase = DocumentPhase::Blocked { requested };
                requested
            }
            DocumentPhase::Active | DocumentPhase::Suspended => document.head(),
        };
        document.failed_checkpoint = Some(requested);
        Ok(requested)
    }

    pub fn checkpoint_retry(
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

    pub fn abort_close(&mut self, id: DocumentId) -> Result<(), DocumentError> {
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

    /// Commit the already-prepared view detach.
    pub fn commit_detach(
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

    /// Idempotent teardown of one view edge.
    pub fn detach_view_if_ready(
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

    /// Atomically detach every document leaf in one close plan: a Ready verdict for every document first, removing nothing, then the infallible removal pass (the multi-document form of `NativeClosePlan`'s `document_ready`/`other_leaf_ready` gate).
    pub fn commit_detach_batch(
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

    /// Read canonical text back through the Surface for conformance tests.
    #[cfg(test)]
    fn surface_text(&self, id: DocumentId) -> Option<String> {
        let document = self.documents.get(&id)?;
        let read = document.surface.read_text(
            &test_authority().grant(aterm_cap::Tier::Trusted),
            aterm_buffer::Range {
                start: LineId(0),
                end: LineId(1),
            },
        );
        read.text.strip_suffix('\n').map(str::to_owned)
    }
}

/// Test-only mint.
#[cfg(test)]
fn test_authority() -> aterm_cap::Authority {
    // SAFETY: `root_authority` requires a trusted launcher before untrusted input; a unit
    // test is one.
    unsafe { aterm_cap::Authority::root_authority() }
}

/// Rebase one byte position through committed edits.
pub fn rebase_position(mut position: usize, deltas: &[EditDelta]) -> usize {
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
        growth =
            growth.saturating_add((delta.inserted_len as isize).saturating_sub(removed as isize));
    }
    position = original.saturating_add_signed(growth);
    position
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> (DocumentStore, DocumentId) {
        let mut store = DocumentStore::for_test();
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
    fn failed_checkpoint_keeps_open_documents_editable() {
        for attached in [false, true] {
            let (mut store, id) = open();
            assert_eq!(store.failed_checkpoint_seq(id), None);
            if attached {
                store.attach_view(id, DocumentViewId(7)).unwrap();
            }
            let phase = store.phase(id);
            assert_eq!(
                phase,
                Some(if attached {
                    DocumentPhase::Active
                } else {
                    DocumentPhase::Suspended
                })
            );
            let checkpoint = store.checkpoint_seq(id);
            let base = store.snapshot(id).unwrap().seq;
            let DocumentTxnOutcome::Committed { seq, .. } = store.transact(
                id,
                base,
                vec![TextEdit {
                    range: 0..0,
                    insert: "x".into(),
                }],
            ) else {
                panic!("the open document accepts an edit");
            };

            assert_eq!(store.checkpoint_fail(id).unwrap(), seq);
            assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
            assert_eq!(store.phase(id), phase);
            assert_eq!(store.checkpoint_seq(id), checkpoint);
            assert_eq!(store.view_count(id), Some(usize::from(attached)));
            let DocumentTxnOutcome::Committed { seq: later, .. } = store.transact(
                id,
                seq,
                vec![TextEdit {
                    range: 0..0,
                    insert: "y".into(),
                }],
            ) else {
                panic!("a failed ordinary checkpoint cannot freeze editing");
            };
            assert!(later > seq);
            assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "yxhello\nworld");
            assert_eq!(store.phase(id), phase);
            assert_eq!(store.dirty(id), Some(true));
            assert_eq!(store.checkpoint_seq(id), checkpoint);
            assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
            assert_eq!(
                store.checkpoint_ack(id, base).unwrap(),
                DocumentCloseReadiness::Pending { requested: later }
            );
            assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
            assert_eq!(
                store.checkpoint_ack(id, seq).unwrap(),
                DocumentCloseReadiness::Pending { requested: later }
            );
            assert_eq!(store.failed_checkpoint_seq(id), None);
            assert_eq!(store.dirty(id), Some(true));
            assert_eq!(
                store.checkpoint_ack(id, later).unwrap(),
                DocumentCloseReadiness::Ready { requested: later }
            );
            assert_eq!(store.phase(id), phase);
            assert_eq!(store.dirty(id), Some(false));
        }
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
        assert_eq!(
            store.prepare_close(id, &[view]).unwrap(),
            DocumentCloseReadiness::Pending { requested: seq }
        );
        assert_eq!(
            store.phase(id),
            Some(DocumentPhase::Closing { requested: seq })
        );
        let late_edit = || {
            vec![TextEdit {
                range: 0..0,
                insert: "late".into(),
            }]
        };
        assert_eq!(
            store.transact(id, seq, late_edit()),
            DocumentTxnOutcome::Rejected(DocumentError::Closing)
        );
        for _ in 0..2 {
            assert_eq!(store.checkpoint_fail(id).unwrap(), seq);
            assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
            assert_eq!(
                store.phase(id),
                Some(DocumentPhase::Blocked { requested: seq })
            );
            assert_eq!(
                store.prepare_close(id, &[view]).unwrap(),
                DocumentCloseReadiness::Blocked { requested: seq }
            );
            assert_eq!(
                store.transact(id, seq, late_edit()),
                DocumentTxnOutcome::Rejected(DocumentError::Closing)
            );
            assert_eq!(store.snapshot(id).unwrap().seq, seq);
            assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "xhello\nworld");
            assert_eq!(store.checkpoint_seq(id), Some(base));
            assert_eq!(store.view_count(id), Some(1));
        }
        store.abort_close(id).unwrap();
        assert_eq!(store.phase(id), Some(DocumentPhase::Active));
        assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
        assert_eq!(
            store.prepare_close(id, &[view]).unwrap(),
            DocumentCloseReadiness::Pending { requested: seq }
        );
        assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
        assert_eq!(store.checkpoint_fail(id).unwrap(), seq);
        assert_eq!(
            store.checkpoint_retry(id).unwrap(),
            DocumentCloseReadiness::Pending { requested: seq }
        );
        assert_eq!(store.failed_checkpoint_seq(id), Some(seq));
        assert_eq!(
            store.phase(id),
            Some(DocumentPhase::Closing { requested: seq })
        );
        assert_eq!(
            store.commit_detach(id, &[view]),
            Err(DocumentError::CloseNotReady)
        );
        assert_eq!(
            store.checkpoint_ack(id, seq).unwrap(),
            DocumentCloseReadiness::Ready { requested: seq }
        );
        assert_eq!(store.failed_checkpoint_seq(id), None);
        assert_eq!(
            store.transact(id, seq, late_edit()),
            DocumentTxnOutcome::Rejected(DocumentError::Closing)
        );
        store.commit_detach(id, &[view]).unwrap();
        assert_eq!(store.view_count(id), Some(0));
        assert_eq!(store.phase(id), Some(DocumentPhase::Suspended));
        assert_eq!(store.snapshot(id).unwrap().text.as_ref(), "xhello\nworld");
    }

    /// An ack past head is refused: `NativeSaveIntentLatch` relies on this for `durable <= head`.
    #[test]
    fn checkpoint_ack_past_head_is_refused_without_moving_the_checkpoint() {
        let (mut store, id) = open();
        let head = store.snapshot(id).unwrap().seq;
        let before = store.checkpoint_seq(id);
        assert_eq!(store.checkpoint_fail(id).unwrap(), head);
        assert_eq!(store.failed_checkpoint_seq(id), Some(head));
        // Even a clean document's failed save remains evidence: the previously
        // durable head is not a new acknowledgement of that failure.
        let older = Seq(head
            .0
            .checked_sub(1)
            .expect("opening appends a spine event"));
        assert_eq!(
            store.checkpoint_ack(id, older).unwrap(),
            DocumentCloseReadiness::Ready { requested: head }
        );
        assert_eq!(store.checkpoint_seq(id), before);
        assert_eq!(store.failed_checkpoint_seq(id), Some(head));
        assert_eq!(
            store.checkpoint_ack(id, Seq(head.0 + 1)),
            Err(DocumentError::CheckpointAheadOfHead)
        );
        assert_eq!(store.checkpoint_seq(id), before);
        assert_eq!(store.failed_checkpoint_seq(id), Some(head));
        assert_eq!(
            store.checkpoint_ack(id, head).unwrap(),
            DocumentCloseReadiness::Ready { requested: head }
        );
        assert_eq!(store.failed_checkpoint_seq(id), None);
    }

    #[test]
    fn batch_detach_is_atomic_across_documents() {
        let mut store = DocumentStore::for_test();
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
    fn replacing_the_text_keeps_positions_outside_the_change_on_their_text() {
        let before = "a = 1\nb = 2\nc = 3\nd = 4\n";
        let after = "a = 1\nc = 3\nd = 4\n";
        let caret = before.find("c = 3").unwrap();
        let later = before.find("d = 4").unwrap() + 2;

        let mut store = DocumentStore::for_test();
        let id = store.open("mem://config".into(), before.into());
        let base = store.snapshot(id).unwrap().seq;
        let DocumentTxnOutcome::Committed { deltas, .. } = store.replace_text(id, base, after)
        else {
            panic!("replace_text did not commit");
        };
        assert_eq!(store.snapshot(id).unwrap().text.as_ref(), after);
        assert_eq!(rebase_position(0, &deltas), 0, "before the change");
        assert_eq!(
            rebase_position(caret, &deltas),
            after.find("c = 3").unwrap(),
            "the line after a deleted one keeps its caret"
        );
        assert_eq!(
            rebase_position(later, &deltas),
            after.find("d = 4").unwrap() + 2
        );

        // The control: the whole-text edit this replaces sends every position to the end.
        let whole = [EditDelta {
            old: 0..before.len(),
            inserted_len: after.len(),
        }];
        assert_eq!(rebase_position(caret, &whole), after.len());
    }

    #[test]
    fn replacing_with_the_same_text_commits_nothing() {
        let (mut store, id) = open();
        let base = store.snapshot(id).unwrap().seq;
        let outcome = store.replace_text(id, base, "hello\nworld");
        assert!(
            matches!(&outcome, DocumentTxnOutcome::Committed { seq, deltas, .. } if *seq == base && deltas.is_empty()),
            "{outcome:?}"
        );
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
