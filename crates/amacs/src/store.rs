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

/// What no reader of a document gets but the person's own eyes on its window (one-mesh design
/// §4.8, "Revealed values stay out of the accessibility tree"): a revealed secret's grid's
/// accessibility tree, the status its editor writes, and every other surface the window hands
/// to another process, carry its concealed cells as "hidden value". Set once, for the document's
/// life ([`DocumentStore::set_concealed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Concealed {
    /// Every cell, headers included, and every line of its text: what a private document whose
    /// opener named no secret columns gets.
    All,
    /// The cells of the grid columns headed by one of these (each header's whole text). The
    /// headers themselves, and the text of every other column, are not concealed; a line of the
    /// document's text still is, since a line holds every column's cell.
    Columns(Vec<String>),
}

impl Concealed {
    /// Whether the cells of the grid column headed `header` are concealed.
    #[must_use]
    pub fn column(&self, header: &str) -> bool {
        match self {
            Self::All => true,
            Self::Columns(headers) => headers.iter().any(|named| named == header),
        }
    }

    /// Whether a header row's cell is concealed: only where every cell is.
    #[must_use]
    pub const fn headers(&self) -> bool {
        matches!(self, Self::All)
    }
}

/// Immutable read projection.
#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub id: DocumentId,
    pub seq: Seq,
    pub text: Arc<str>,
}

/// A read-only directory projection. Entry ranges refer to the displayed text;
/// navigation uses the exact URI, never a displayed (possibly escaped) filename.
#[derive(Clone, Debug)]
pub struct DirectoryListing {
    pub text: String,
    pub parent: Option<String>,
    pub entries: Vec<(Range<usize>, String)>,
}

impl DirectoryListing {
    /// The exact navigation target on the line containing `point`.
    pub fn target_at(&self, point: usize) -> Option<&str> {
        self.entries
            .iter()
            .find(|(range, _)| range.contains(&point))
            .map(|(_, uri)| uri.as_str())
    }

    /// Keep point on a surviving entry after refresh, otherwise use the first entry.
    pub fn point_for(&self, target: Option<&str>) -> usize {
        self.entries
            .iter()
            .find(|(_, uri)| Some(uri.as_str()) == target)
            .or_else(|| self.entries.first())
            .map_or(0, |(range, _)| range.start)
    }
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
    ReadOnly,
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
    directory: Option<DirectoryListing>,
    /// Opened to be read and never changed ([`DocumentStore::set_read_only`]): every transaction
    /// is refused, as a directory listing's is.
    read_only: bool,
    /// What no reader but the person's eyes gets of it ([`DocumentStore::set_concealed`]).
    concealed: Option<Concealed>,
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

    /// Removing `closing` leaves another view, or detaches a document whose
    /// head is already durable.
    fn close_ready(&self, closing: &[DocumentViewId]) -> Result<bool, DocumentError> {
        if closing.iter().any(|view| !self.views.contains_key(view)) {
            return Err(DocumentError::UnknownView);
        }
        let closing_set = closing.iter().copied().collect::<BTreeSet<_>>();
        let remaining = self
            .views
            .keys()
            .filter(|view| !closing_set.contains(view))
            .count();
        Ok(remaining > 0 || !self.dirty())
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
            directory: None,
            read_only: false,
            concealed: None,
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

    pub fn directory(&self, id: DocumentId) -> Option<&DirectoryListing> {
        self.documents.get(&id)?.directory.as_ref()
    }

    /// Make document `id` read-only for the rest of its life: from now on every transaction on
    /// it is refused ([`DocumentError::ReadOnly`]). Nothing clears it: opening its URI again
    /// reuses the read-only document.
    ///
    /// # Errors
    /// No such document.
    pub fn set_read_only(&mut self, id: DocumentId) -> Result<(), DocumentError> {
        self.documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?
            .read_only = true;
        Ok(())
    }

    /// Conceal `concealed` of document `id` from every reader but the person's eyes, for the rest
    /// of its life ([`Concealed`]). A later call keeps the first: what a reader may be told never
    /// widens under it.
    ///
    /// # Errors
    /// No such document.
    pub fn set_concealed(
        &mut self,
        id: DocumentId,
        concealed: Concealed,
    ) -> Result<(), DocumentError> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        document.concealed.get_or_insert(concealed);
        Ok(())
    }

    /// What of document `id` no reader but the person's eyes gets ([`Self::set_concealed`]);
    /// `None` for a document that conceals nothing.
    #[must_use]
    pub fn concealed(&self, id: DocumentId) -> Option<&Concealed> {
        self.documents.get(&id)?.concealed.as_ref()
    }

    /// Whether document `id` refuses every change: a directory listing, or a document made
    /// read-only ([`Self::set_read_only`]).
    #[must_use]
    pub fn read_only(&self, id: DocumentId) -> bool {
        self.documents
            .get(&id)
            .is_some_and(|document| document.directory.is_some() || document.read_only)
    }

    /// Replace a generated listing without making it a user edit or a dirty buffer.
    pub fn set_directory(
        &mut self,
        id: DocumentId,
        listing: DirectoryListing,
    ) -> Result<(), DocumentError> {
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        if !matches!(
            document.phase,
            DocumentPhase::Active | DocumentPhase::Suspended
        ) {
            return Err(DocumentError::Closing);
        }
        if document.projection.as_ref() == listing.text {
            document.directory = Some(listing);
            document.checkpoint_seq = document.head();
            return Ok(());
        }
        let previous = document.directory.take();
        let base = document.head();
        let result = self.replace_text(id, base, &listing.text);
        let document = self
            .documents
            .get_mut(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        match result {
            DocumentTxnOutcome::Committed { .. } => {
                document.checkpoint_seq = document.head();
                document.directory = Some(listing);
                Ok(())
            }
            DocumentTxnOutcome::Rejected(error) => {
                document.directory = previous;
                Err(error)
            }
            DocumentTxnOutcome::Conflict { .. } => {
                unreachable!("exclusive store borrow preserves sequence")
            }
        }
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

    /// The number of attached views, including in release builds where hosts use it
    /// to discard work whose last view has closed.
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
        if document.directory.is_some() || document.read_only {
            return DocumentTxnOutcome::Rejected(DocumentError::ReadOnly);
        }
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

    /// Whether removing `closing` needs no checkpoint first: [`Self::prepare_close`]
    /// would answer Ready. Asking starts no close and changes no phase.
    pub fn close_ready(
        &self,
        id: DocumentId,
        closing: &[DocumentViewId],
    ) -> Result<bool, DocumentError> {
        let document = self
            .documents
            .get(&id)
            .ok_or(DocumentError::UnknownDocument)?;
        document.close_ready(closing)
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
        let requested = document.head();
        if document.close_ready(closing)? {
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
        if !self.close_ready(id, closing)? {
            return Err(DocumentError::CloseNotReady);
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
        let mut grouped = BTreeMap::<DocumentId, Vec<DocumentViewId>>::new();
        for (document, views) in closing {
            let entry = grouped.entry(*document).or_default();
            for &view in views {
                if !entry.contains(&view) {
                    entry.push(view);
                }
            }
        }
        for (document, views) in &grouped {
            if !self.close_ready(*document, views)? {
                return Err(DocumentError::CloseNotReady);
            }
        }

        for (document, views) in &grouped {
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
pub fn rebase_position(position: usize, deltas: &[EditDelta]) -> usize {
    let original = position;
    let shift = |base: usize, inserted: usize, removed: usize| {
        if inserted >= removed {
            base.saturating_add(inserted - removed)
        } else {
            base.saturating_sub(removed - inserted)
        }
    };
    let mut removed = 0usize;
    let mut inserted = 0usize;
    for delta in deltas {
        if original < delta.old.start {
            break;
        }
        if original <= delta.old.end {
            return shift(
                delta.old.start,
                inserted.saturating_add(delta.inserted_len),
                removed,
            );
        }
        removed = removed.saturating_add(delta.old.end.saturating_sub(delta.old.start));
        inserted = inserted.saturating_add(delta.inserted_len);
    }
    shift(original, inserted, removed)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
