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
    /// Resolved file paths for the URI (as given, codec-decoded, and canonicalized).
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
                .and_then(|document| self.document_store.snapshot(*document))
                .map_or(DocAnswer::Unknown, |snapshot| {
                    DocAnswer::Text(DocText {
                        doc: snapshot.id.get(),
                        seq: snapshot.seq.0,
                        text: snapshot.text,
                    })
                }),
        }
    }

    fn editor_documents(&self) -> BTreeMap<DocumentId, Vec<(WindowId, ViewId)>> {
        let mut documents: BTreeMap<DocumentId, Vec<(WindowId, ViewId)>> = BTreeMap::new();
        for (wid, ws) in &self.windows {
            for tab in ws.tab_set.tabs() {
                for view in tab.root.leaves() {
                    let Some(View::Native(native)) = self.view_store.get(view).copied() else {
                        continue;
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
                }
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
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::Receiver;

    use amacs::protocol::edit::{AwaitKey, EditOk, EditRequest};
    use amacs::protocol::read::{DOC_READ_CAP, LineRange, format_lines};

    use super::*;
    use crate::control_edit::{AwaitCall, EditCall, WaiterId};
    use crate::native_editor::Selection;
    use crate::tab_model::TabId;

    struct Fixture {
        _dir: aterm_tempfile::TempDir,
        path: PathBuf,
        /// The file's URI as a client spells it (`/var/…`, not `/private/var/…`).
        uri: String,
    }

    fn fixture(name: &str, bytes: &[u8]) -> Fixture {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-doc-{name}-"))
            .tempdir()
            .unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        Fixture {
            _dir: dir,
            path,
            uri,
        }
    }

    fn canonical(path: &Path) -> String {
        crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(path).unwrap())
            .unwrap()
    }

    fn app() -> App {
        App::headless_for_test()
    }

    fn doc(app: &App, rest: &str) -> String {
        crate::control_doc::serve_doc(rest, &mut |query| Ok(app.doc_answer(query, Instant::now())))
    }

    fn rows(reply: &str) -> Vec<DocRow> {
        let mut lines = reply.lines();
        let count: usize = lines
            .next()
            .and_then(|header| header.strip_prefix("OK "))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("an `OK <n>` header: {reply:?}"));
        let rows: Vec<DocRow> = lines
            .map(|line| DocRow::parse(line).unwrap_or_else(|error| panic!("{line}: {error}")))
            .collect();
        assert_eq!(rows.len(), count, "{reply}");
        rows
    }

    fn body(reply: &str) -> (u64, &str) {
        let (header, body) = reply
            .split_once('\n')
            .unwrap_or_else(|| panic!("a header line: {reply:?}"));
        let mut words = header.split_whitespace();
        assert_eq!(words.next(), Some("OK"), "{reply}");
        let nbytes: usize = words.next().unwrap().parse().unwrap();
        assert_eq!(nbytes, body.len(), "the header counts the body");
        let doc = words
            .next()
            .and_then(|word| word.strip_prefix("doc="))
            .and_then(|id| id.parse().ok())
            .unwrap_or_else(|| panic!("doc= in {header}"));
        assert_eq!(words.next(), None, "{header}");
        (doc, body)
    }

    fn open(app: &mut App, f: &Fixture) -> ViewId {
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        app.active_native_view(WindowId(0)).unwrap().1
    }

    fn document_of(app: &App, view: ViewId) -> DocumentId {
        let Some(View::Native(native)) = app.view_store.get(view).copied() else {
            panic!("view {view:?} is not a native view");
        };
        app.native_runtime.document_id(native.instance).unwrap()
    }

    fn set_selection(app: &mut App, view: ViewId, anchor: usize, head: usize) {
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
            panic!("an Editor view");
        };
        let buffer = state.buffer.as_mut().unwrap();
        buffer.selections = vec![Selection { anchor, head }];
        buffer.primary = 0;
    }

    fn tab_of(app: &App, view: ViewId) -> TabId {
        app.windows[&WindowId(0)]
            .tab_set
            .tabs()
            .iter()
            .find(|tab| tab.root.contains(view))
            .map(|tab| tab.id)
            .expect("the view's tab")
    }

    fn select(app: &mut App, tab: TabId) {
        let wid = WindowId(0);
        assert!(app.windows.get_mut(&wid).unwrap().tab_set.switch_to(tab));
        app.sync_window(wid);
    }

    fn caller(app: &mut App) -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(3 << 40);
        let sid = NEXT.fetch_add(1 << 12, Ordering::Relaxed);
        app.next_session_id = sid;
        app.push_stub_tab(WindowId(0), crate::stub_session(sid));
        app.next_session_id = sid + 1;
        sid
    }

    fn edit_waited(app: &mut App, session: u64, uri: &str) -> (EditOk, Receiver<String>) {
        app.serve_edits_headless_for_test();
        let ticket = crate::control_edit::mint_ticket();
        let (reply, answer) = std::sync::mpsc::channel();
        let request = EditRequest {
            uri: uri.to_string(),
            line: None,
            create: false,
            for_word: None,
            wait: true,
        };
        app.begin_edit(EditCall {
            ticket,
            session,
            real_uri: crate::control_edit::real_file_uri(uri),
            request,
            reply,
        });
        let line = answer.try_recv().expect("an inline edit answers at once");
        let ok = EditOk::parse(&line).unwrap_or_else(|error| panic!("{line:?}: {error}"));
        static WAITER: AtomicU64 = AtomicU64::new(3 << 40);
        let (reply, parked) = std::sync::mpsc::channel();
        app.await_edit(AwaitCall {
            waiter: WaiterId(WAITER.fetch_add(1, Ordering::Relaxed)),
            key: AwaitKey::Ticket(ticket),
            real_uri: None,
            reply,
        });
        (ok, parked)
    }

    #[test]
    fn doc_ls_lists_views_cursor_and_selection() {
        let text = "alpha\nbeta gamma\ndelta\n";
        let f = fixture("notes.txt", text.as_bytes());
        let mut app = app();
        let session = caller(&mut app);
        let caller_tab = app.windows[&WindowId(0)].tab_set.active_id().unwrap();
        let person = open(&mut app, &f);
        let gamma = text.find("gamma").unwrap();
        set_selection(&mut app, person, gamma, gamma + "gamma".len());
        select(&mut app, caller_tab);
        let (edit, _waiting) = edit_waited(&mut app, session, &f.uri);
        let edit_view = ViewId::from_stored(edit.view);
        let person_tab = tab_of(&app, person);
        select(&mut app, person_tab);
        let doc_id = document_of(&app, person).get();
        assert_eq!(edit.doc, doc_id, "one document, two views");

        let listed = rows(&doc(&app, "ls"));
        let mut views = vec![person.get(), edit_view.get()];
        views.sort_unstable();
        let row = DocRow {
            doc: doc_id,
            uri: canonical(&f.path),
            seq: app
                .document_store
                .snapshot(document_of(&app, person))
                .unwrap()
                .seq
                .0,
            dirty: false,
            disk: DiskState::Same,
            views,
            tickets: vec![edit.ticket],
            cursor: Some(ViewPoint {
                view: person.get(),
                at: Point { line: 2, col: 11 },
            }),
            sel: Some(ViewSpan {
                view: person.get(),
                from: Point { line: 2, col: 6 },
                to: Point { line: 2, col: 11 },
            }),
        };
        assert_eq!(listed, vec![row.clone()]);
        let foreign = (
            session + 1_000,
            hold::Hold {
                ticket: crate::control_edit::mint_ticket(),
                view: person.get(),
            },
        );
        hold::set(foreign.0, foreign.1);
        assert_eq!(rows(&doc(&app, "ls"))[0].tickets, vec![edit.ticket]);
        hold::clear(foreign.0, foreign.1.ticket);
        assert_eq!(
            rows(&doc(&app, &format!("ls uri={}", f.uri))),
            vec![row.clone()]
        );
        let other = fixture("other.txt", b"x\n");
        assert_eq!(
            rows(&doc(&app, &format!("ls uri={}", other.uri))),
            Vec::new()
        );
        let markdown = fixture("readme.md", b"# hi\n");
        app.open_document_tab(AppKind::Markdown, &markdown.uri)
            .unwrap();
        assert_eq!(rows(&doc(&app, "ls")), vec![row.clone()]);

        let edit_tab = tab_of(&app, edit_view);
        select(&mut app, edit_tab);
        let in_front = rows(&doc(&app, "ls"));
        assert_eq!(
            in_front[0].cursor,
            Some(ViewPoint {
                view: edit_view.get(),
                at: Point::START
            })
        );
        assert_eq!(in_front[0].sel, None, "a caret selects nothing");

        app.dispatch_native_event(
            WindowId(0),
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
                "X".to_string(),
            )),
        )
        .unwrap();
        let typed = rows(&doc(&app, "ls")).remove(0);
        assert!(typed.dirty && typed.seq > row.seq, "{typed:?}");
        assert_eq!(typed.cursor.unwrap().at, Point { line: 1, col: 2 });

        std::fs::write(&f.path, b"rewritten\n").unwrap();
        assert_eq!(rows(&doc(&app, "ls"))[0].disk, DiskState::Changed);
        std::fs::remove_file(&f.path).unwrap();
        assert_eq!(rows(&doc(&app, "ls"))[0].disk, DiskState::Missing);
        let gone = rows(&doc(&app, &format!("ls uri={}", f.uri)));
        assert_eq!(
            gone.iter()
                .map(|row| (row.doc, row.disk))
                .collect::<Vec<_>>(),
            vec![(doc_id, DiskState::Missing)]
        );
        let read = doc(&app, &format!("read {} lines=1", f.uri));
        assert_eq!(body(&read).0, doc_id, "{read}");
        hold::clear(session, edit.ticket);
    }

    #[test]
    fn doc_read_prints_anchors_matching_fnv1a() {
        let bytes = b"fn main() {\r\n    run();\r\n}\r\n";
        let f = fixture("main.rs", bytes);
        let mut app = app();
        let view = open(&mut app, &f);
        let document = document_of(&app, view);
        let snapshot = app.document_store.snapshot(document).unwrap();

        let (doc_id, read) = {
            let reply = doc(&app, &format!("read {} lines=1-2", f.uri));
            let (doc_id, body) = body(&reply);
            (doc_id, body.to_string())
        };
        assert_eq!(doc_id, document.get());
        let h6 =
            |line: &str| format!("{:016x}", aterm_hash::fnv1a_64(line.as_bytes()))[..6].to_string();
        assert_eq!(
            read,
            format!(
                "1:{}|fn main() {{\n2:{}|    run();\n-- lines=3 seq={}\n",
                h6("fn main() {"),
                h6("    run();"),
                snapshot.seq.0
            )
        );
        let decoded = crate::native_document_io::Decoded::hold(
            String::from_utf8(bytes.to_vec()).unwrap(),
            false,
        );
        let range = LineRange {
            first: 1,
            last: Some(2),
        };
        assert_eq!(
            read,
            format_lines(&decoded.text, snapshot.seq.0, range, DOC_READ_CAP)
        );
        let by_id = doc(&app, &format!("read doc={} lines=1-2", document.get()));
        assert_eq!(body(&by_id).1, read);
        let whole = doc(&app, &format!("read {}", document.get()));
        assert!(body(&whole).1.contains("\n3:"), "{whole}");
        assert!(
            body(&whole)
                .1
                .ends_with(&format!("-- lines=3 seq={}\n", snapshot.seq.0))
        );
    }

    #[test]
    fn doc_read_caps_at_256k_with_next() {
        let text: String = (1..=12_000)
            .map(|n| format!("line {n:05} of the long file\n"))
            .collect();
        let f = fixture("long.txt", text.as_bytes());
        let mut app = app();
        let view = open(&mut app, &f);
        let document = document_of(&app, view).get();

        let mut rows_read = Vec::new();
        let mut asked = 1;
        let mut pages = 0;
        loop {
            let reply = doc(&app, &format!("read {document} lines={asked}-"));
            let (_, page) = body(&reply);
            let (lines, trailer) = page.rsplit_once("-- ").unwrap();
            assert!(
                lines.len() <= DOC_READ_CAP,
                "{} bytes of lines",
                lines.len()
            );
            pages += 1;
            rows_read.extend(lines.lines().map(str::to_string));
            let next = trailer
                .split_whitespace()
                .find_map(|word| word.strip_prefix("next="));
            match next {
                Some(next) => {
                    let next: usize = next.parse().unwrap();
                    assert_eq!(
                        next,
                        rows_read.len() + 1,
                        "next= is the first line not read"
                    );
                    asked = next;
                }
                None => break,
            }
            assert!(pages < 10, "paging moves on");
        }
        assert!(pages >= 2, "the file is past one read's cap");
        let whole = format_lines(&text, 0, LineRange::ALL, usize::MAX);
        let expected: Vec<&str> = whole.lines().filter(|row| !row.starts_with("--")).collect();
        assert_eq!(rows_read, expected);
    }

    #[test]
    fn doc_read_unknown_ref_errs() {
        let f = fixture("one.txt", b"one\n");
        let markdown = fixture("two.md", b"# two\n");
        let mut app = app();
        let view = open(&mut app, &f);
        let known = document_of(&app, view).get();
        app.open_document_tab(AppKind::Markdown, &markdown.uri)
            .unwrap();
        let shown = app.active_native_view(WindowId(0)).unwrap().1;
        let markdown_doc = document_of(&app, shown).get();
        let missing = fixture("three.txt", b"three\n");
        let unknown = known + markdown_doc + 100;
        for (rest, answer) in [
            (
                format!("read {unknown}"),
                format!("ERR unknown doc {unknown}\n"),
            ),
            (
                format!("read {}", missing.uri),
                format!("ERR unknown doc {}\n", missing.uri),
            ),
            (
                format!("read {markdown_doc}"),
                format!("ERR unknown doc {markdown_doc}\n"),
            ),
            (
                "read file://host/x".to_string(),
                "ERR bad uri=file://host/x (remote file URI authorities are not allowed)\n"
                    .to_string(),
            ),
            (
                "read".to_string(),
                format!("ERR usage: {}\n", amacs::protocol::doc::DOC_USAGE),
            ),
            (
                "read notes.txt".to_string(),
                "ERR bad doc=notes.txt\n".to_string(),
            ),
            (
                format!("apply {known} len=3"),
                "ERR unsupported doc apply: not served yet\n".to_string(),
            ),
            (
                format!("save {known}"),
                "ERR unsupported doc save: not served yet\n".to_string(),
            ),
        ] {
            assert_eq!(doc(&app, &rest), answer, "doc {rest}");
        }
        assert!(doc(&app, &format!("read {known}")).starts_with("OK "));
    }
}
