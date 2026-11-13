// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The main thread's half of the `doc` verb's read forms (amacs design §7c; slice A1).

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Instant;

use amacs::protocol::doc::{DiskState, DocRow, Point, ViewPoint, ViewSpan};
use amacs::protocol::edit::Ticket;

use crate::control_edit::hold;
use crate::document_store::DocumentId;
use crate::native_app::{AppKind, AppViewState};
use crate::native_document_io::ObservedFileVersion;
use crate::tab_model::{View, ViewId};
use crate::{App, WindowId};

/// The document a `doc read` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DocNames {
    /// Its `doc=` id.
    Id(u64),
    /// The file's URI spellings: as given, the codec's, and the realpath one the store keys by.
    Uri(Vec<String>),
}

/// What a `doc` read asks the main thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DocQuery {
    /// `doc ls`: every live document, or the one a `uri=` names.
    Ls(Option<Vec<String>>),
    /// `doc read`: one live document's text.
    Read(DocNames),
}

/// One `doc` read on its way to the main thread, with the channel its answer goes back on.
#[derive(Clone, Debug)]
pub(crate) struct DocCall {
    pub(crate) query: DocQuery,
    pub(crate) answer: Sender<DocAnswer>,
}

impl PartialEq for DocCall {
    fn eq(&self, other: &Self) -> bool {
        self.query == other.query
    }
}

impl Eq for DocCall {}

/// The main thread's answer to a [`DocQuery`].
#[derive(Debug)]
pub(crate) enum DocAnswer {
    /// `doc ls`'s documents, ascending by id.
    Listed(Vec<DocFacts>),
    /// `doc read`'s document.
    Text(DocText),
    /// No live document is the one asked for.
    Unknown,
    /// `doc read` of a private edit's document
    /// ([`crate::native_document_journal::private_uri`]), whose text the editor never serves.
    Refused,
}

/// One live document, as `doc ls` lists it.
#[derive(Debug)]
pub(crate) struct DocFacts {
    /// Its row, `disk=` still [`DiskState::Unknown`] and `cursor=`/`sel=` still unset.
    pub(crate) row: DocRow,
    /// Where the person's caret is; `None` when no view has one.
    pub(crate) caret: Option<Caret>,
    /// What `disk=` compares; `None` when the document has no file version to compare with.
    pub(crate) disk: Option<DiskProbe>,
}

/// Caret and selection in one document view as byte offsets into `text` at `seq=`.
#[derive(Debug)]
pub(crate) struct Caret {
    pub(crate) view: u64,
    pub(crate) head: usize,
    /// The primary selection, empty for a caret.
    pub(crate) selection: Range<usize>,
    pub(crate) text: Arc<str>,
}

impl DocFacts {
    /// Fill in the row's `cursor=` and `sel=` from [`Self::caret`].
    pub(crate) fn place(&mut self) {
        let Some(caret) = &self.caret else {
            return;
        };
        let at = |offset| Point::at(&caret.text, offset);
        self.row.cursor = Some(ViewPoint {
            view: caret.view,
            at: at(caret.head),
        });
        self.row.sel = (!caret.selection.is_empty()).then(|| ViewSpan {
            view: caret.view,
            from: at(caret.selection.start),
            to: at(caret.selection.end),
        });
    }
}

/// A document's file and the version its buffer was read or last saved as.
#[derive(Debug)]
pub(crate) struct DiskProbe {
    /// The file the document saves to (its symlinks resolved).
    pub(crate) path: PathBuf,
    pub(crate) baseline: ObservedFileVersion,
}

/// A live document's text at one sequence, shared with the store.
#[derive(Debug)]
pub(crate) struct DocText {
    pub(crate) doc: u64,
    pub(crate) seq: u64,
    pub(crate) text: Arc<str>,
}

impl App {
    /// [`crate::app_control::InspectRequest::Doc`]: answer `call` on its own channel.
    pub(crate) fn answer_doc(&self, call: DocCall) {
        let _ = call
            .answer
            .send(self.doc_answer(call.query, Instant::now()));
    }

    /// `query`'s answer, the edit holds read as of `now`.
    pub(crate) fn doc_answer(&self, query: DocQuery, now: Instant) -> DocAnswer {
        let live = self.editor_documents();
        match query {
            DocQuery::Ls(names) => DocAnswer::Listed(
                live.iter()
                    .filter(|(document, _)| {
                        names
                            .as_ref()
                            .is_none_or(|names| self.document_spelled(**document, names))
                    })
                    .filter_map(|(document, views)| self.doc_facts(*document, views, now))
                    .collect(),
            ),
            DocQuery::Read(names) => live
                .keys()
                .find(|document| match &names {
                    DocNames::Id(id) => document.get() == *id,
                    DocNames::Uri(spellings) => self.document_spelled(**document, spellings),
                })
                .map_or(DocAnswer::Unknown, |document| {
                    if self
                        .document_store
                        .canonical_uri(*document)
                        .is_some_and(crate::native_document_journal::private_uri)
                    {
                        return DocAnswer::Refused;
                    }
                    self.document_store
                        .snapshot(*document)
                        .map_or(DocAnswer::Unknown, |snapshot| {
                            DocAnswer::Text(DocText {
                                doc: snapshot.id.get(),
                                seq: snapshot.seq.0,
                                text: snapshot.text,
                            })
                        })
                }),
        }
    }

    fn editor_documents(&self) -> BTreeMap<DocumentId, Vec<(WindowId, ViewId)>> {
        let mut documents: BTreeMap<DocumentId, Vec<(WindowId, ViewId)>> = BTreeMap::new();
        for (wid, ws) in &self.windows {
            for tab in ws.tab_set.tabs() {
                tab.root.visit(&mut |&view| {
                    let Some(View::Native(native)) = self.view_store.get(view).copied() else {
                        return;
                    };
                    let editor = self
                        .native_runtime
                        .app(native.instance)
                        .is_some_and(|app| app.kind() == AppKind::Editor);
                    if let Some(document) = self
                        .native_runtime
                        .document_id(native.instance)
                        .filter(|_| editor)
                    {
                        documents.entry(document).or_default().push((*wid, view));
                    }
                });
            }
        }
        for views in documents.values_mut() {
            views.sort_by_key(|(_, view)| view.get());
        }
        documents
    }

    fn document_spelled(&self, document: DocumentId, spellings: &[String]) -> bool {
        self.document_store
            .canonical_uri(document)
            .is_some_and(|uri| spellings.iter().any(|spelling| spelling == uri))
    }

    fn doc_facts(
        &self,
        document: DocumentId,
        views: &[(WindowId, ViewId)],
        now: Instant,
    ) -> Option<DocFacts> {
        let snapshot = self.document_store.snapshot(document)?;
        let uri = self.document_store.canonical_uri(document)?.to_string();
        let ids: Vec<u64> = views.iter().map(|(_, view)| view.get()).collect();
        let caret = self.person_view(views).and_then(|view| {
            let Some(AppViewState::Editor(state)) = self.native_runtime.view_state(view) else {
                return None;
            };
            let selection = state.buffer.as_ref()?.primary_selection();
            Some(Caret {
                view: view.get(),
                head: selection.head,
                selection: selection.range(),
                text: Arc::clone(&snapshot.text),
            })
        });
        Some(DocFacts {
            row: DocRow {
                doc: document.get(),
                uri,
                seq: snapshot.seq.0,
                dirty: self.document_store.dirty(document).unwrap_or(false),
                disk: DiskState::Unknown,
                tickets: self.tickets_through(&ids, now),
                views: ids,
                cursor: None,
                sel: None,
            },
            caret,
            disk: self.disk_probe(document),
        })
    }

    fn tickets_through(&self, views: &[u64], now: Instant) -> Vec<Ticket> {
        let mut tickets: Vec<Ticket> = hold::holds_through(views, now)
            .into_iter()
            .filter(|(session, _)| self.session_by_id(*session).is_some())
            .map(|(_, ticket)| ticket)
            .collect();
        tickets.sort_unstable();
        tickets.dedup();
        tickets
    }

    fn person_view(&self, views: &[(WindowId, ViewId)]) -> Option<ViewId> {
        let front = |wid: WindowId| self.active_native_view(wid).map(|(_, view)| view);
        self.frontmost_window
            .and_then(front)
            .filter(|view| views.iter().any(|(_, shown)| shown == view))
            .or_else(|| {
                views
                    .iter()
                    .find(|(wid, view)| front(*wid) == Some(*view))
                    .map(|(_, view)| *view)
            })
            .or_else(|| views.first().map(|(_, view)| *view))
    }

    fn disk_probe(&self, document: DocumentId) -> Option<DiskProbe> {
        let persistence = &self.native_documents.persistence;
        let grant = self
            .native_documents
            .grants
            .get(persistence.grant(document)?)?;
        Some(DiskProbe {
            path: grant.target().target_path().to_path_buf(),
            baseline: persistence.observed(document)?,
        })
    }
}

#[cfg(test)]
#[path = "app_doc_tests.rs"]
mod tests;
