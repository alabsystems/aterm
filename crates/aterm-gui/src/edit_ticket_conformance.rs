// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for `edit` tickets against `amacs_edit_ticket_model` ([`Rig::project`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Instant;

use amacs::protocol::edit::{AwaitKey, EditOutcome, EditRequest, EditResult, Ticket};
use aterm_models_native::models_amacs_ticket::amacs_edit_ticket_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use super::{Ending, ORPHAN_AFTER, Phase};
use crate::app_documents::{DocumentAdmissionOutcome, DocumentOpenFailure};
use crate::control_edit::{AwaitCall, EditCall, WaiterId};
use crate::document_store::DocumentId;
use crate::native_app::{AppEvent, AppKind, TextInputEvent};
use crate::native_editor::EditorCommand;
use crate::tab_model::{TabId, ViewId};
use crate::{App, WindowId};

/// One run: the App, the ticket and the views it watches, and the ghosts the
/// projection keeps once the real state is gone.
pub(crate) struct Rig {
    app: App,
    wid: WindowId,
    session: u64,
    caller_tab: TabId,
    _dir: aterm_tempfile::TempDir,
    uri: String,
    /// The ticket's file, and its identity and bytes when last read.
    path: PathBuf,
    file: Option<(Option<u128>, Vec<u8>)>,
    ticket: Option<Ticket>,
    edit_view: Option<ViewId>,
    person_view: Option<ViewId>,
    document: Option<DocumentId>,
    /// The checkpoint the document opened with: the model's `0`.
    base: u64,
    /// The last head and checkpoint read while the document had a view.
    head: u64,
    durable: u64,
    start: i64,
    close_seq: i64,
    finish_seq: i64,
    writes: i64,
    abort_writes: i64,
    lost: i64,
    /// The rig's own parked `await edit` channels and the outcome lines they
    /// delivered.
    waits: Vec<Receiver<String>>,
    answers: Vec<String>,
    /// The `edit` request's own reply channel.
    reply: Option<Receiver<String>>,
    next_waiter: u64,
    model: Model,
    state: State,
    trace: Vec<String>,
}

impl Rig {
    fn new(name: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-edit-conf-{name}-"))
            .tempdir()
            .unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "base\n").unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let file = read_file(&path);
        let mut app = super::tests::app();
        let wid = WindowId(0);
        let session = super::tests::caller(&mut app, wid);
        let caller_tab = super::tests::active_tab(&app, wid);
        let model = amacs_edit_ticket_model();
        let state = model.init_state();
        let rig = Rig {
            app,
            wid,
            session,
            caller_tab,
            _dir: dir,
            uri,
            path,
            file,
            ticket: None,
            edit_view: None,
            person_view: None,
            document: None,
            base: 0,
            head: 0,
            durable: 0,
            start: 0,
            close_seq: 0,
            finish_seq: 0,
            writes: 0,
            abort_writes: 0,
            lost: 0,
            waits: Vec::new(),
            answers: Vec::new(),
            reply: None,
            next_waiter: 1,
            model,
            state,
            trace: Vec::new(),
        };
        let first = rig.project();
        assert_eq!(first, rig.state, "a fresh App projects to the model's Init");
        rig
    }

    /// The real state, as the model's variables.
    pub(crate) fn project(&self) -> State {
        let edits = &self.app.native_documents.edits;
        let ring = |ticket: Ticket| -> Vec<EditOutcome> {
            edits
                .resolved
                .iter()
                .filter(|resolved| resolved.ticket == ticket)
                .map(|resolved| resolved.outcome)
                .collect()
        };
        let (head, durable) = self.numbers();
        let foreign = self
            .ticket
            .and_then(|ticket| edits.open.get(&ticket))
            .is_some_and(|entry| {
                matches!(entry.phase, Phase::Open { .. })
                    && entry.holds_foreign_text(Some(aterm_buffer::Seq(durable)))
            });
        let (state, waiter, outcomes, outcome) = match self.ticket {
            None => (0, 0, 0, None),
            Some(ticket) => {
                let recorded = ring(ticket);
                let state = match edits.open.get(&ticket) {
                    Some(entry) => match entry.phase {
                        Phase::Admitting { .. } => 1,
                        Phase::Open { .. } if entry.orphaned => 3,
                        Phase::Open { .. } => 2,
                    },
                    None if recorded.is_empty() => 5,
                    None => 4,
                };
                (
                    state,
                    i64::from(edits.waited(ticket)),
                    recorded.len() as i64,
                    recorded.last().copied(),
                )
            }
        };
        let linked = |view: Option<ViewId>| {
            i64::from(view.is_some_and(|view| self.app.view_store.get(view).is_some()))
        };
        let closing = self.document.is_some_and(|document| {
            linked(self.edit_view) == 1 && self.app.document_close_pending_for_test(document)
        });
        let relative = |seq: u64| seq as i64 - self.base as i64;
        let ending = self
            .edit_view
            .filter(|_| linked(self.edit_view) == 1)
            .and_then(|view| edits.endings.get(&view).copied());
        let result = outcome.map_or(0, |outcome| match outcome.result {
            EditResult::Closed => 1,
            EditResult::Finished => 2,
            EditResult::Aborted => 3,
        });
        [
            ("state", state),
            ("view", linked(self.edit_view)),
            ("other", linked(self.person_view)),
            ("head", relative(head)),
            ("durable", relative(durable)),
            ("start", self.start),
            ("closing", i64::from(closing)),
            ("close_seq", self.close_seq),
            ("waiter", waiter),
            ("outcomes", outcomes),
            ("answered", i64::from(!self.answers.is_empty())),
            (
                "saved",
                outcome.map_or(0, |outcome| i64::from(outcome.saved)),
            ),
            (
                "answer_seq",
                outcome.map_or(0, |outcome| relative(outcome.seq)),
            ),
            ("result", result),
            (
                "finishing",
                i64::from(matches!(ending, Some(Ending::Finish { .. }))),
            ),
            ("finish_seq", self.finish_seq),
            (
                "aborting",
                i64::from(matches!(ending, Some(Ending::Abort { .. }))),
            ),
            ("writes", self.writes),
            ("abort_writes", self.abort_writes),
            ("foreign", i64::from(foreign)),
            ("lost", self.lost),
        ]
        .into_iter()
        .collect()
    }

    /// The document's head and durable checkpoint now, or the last ones read
    /// once the store no longer has it.
    fn numbers(&self) -> (u64, u64) {
        let Some(document) = self.document else {
            return (self.head, self.durable);
        };
        let store = &self.app.document_store;
        match (store.snapshot(document), store.checkpoint_seq(document)) {
            (Some(snapshot), Some(durable)) => (snapshot.seq.0, durable.0),
            _ => (self.head, self.durable),
        }
    }

    /// Read what the real step left behind that the projection keeps: the
    /// document and its opening checkpoint, the ticket's view and start, the
    /// numbers while they can be read, and the answers delivered.
    fn settle(&mut self) {
        let open = self
            .ticket
            .and_then(|ticket| self.app.native_documents.edits.open.get(&ticket));
        if let Some(entry) = open
            && let Phase::Open {
                view,
                document,
                start,
                ..
            } = entry.phase
        {
            self.edit_view = Some(view);
            if self.document.is_none() {
                self.adopt_document(document);
            }
            self.start = start.0 as i64 - self.base as i64;
        }
        if let Some(view) = self.person_view
            && self.document.is_none()
            && let Some(crate::tab_model::View::Native(native)) =
                self.app.view_store.get(view).copied()
            && let Some(document) = self.app.native_runtime.document_id(native.instance)
        {
            self.adopt_document(document);
        }
        (self.head, self.durable) = self.numbers();
        if let Some(view) = self.edit_view
            && let Some(Ending::Finish { seq, .. }) =
                self.app.native_documents.edits.endings.get(&view).copied()
        {
            self.finish_seq = seq.0 as i64 - self.base as i64;
        }
        let file = read_file(&self.path);
        if file != self.file {
            if self.state["view"] == 1 {
                self.writes += 1;
            }
            self.file = file;
        }
        for wait in &self.waits {
            while let Ok(line) = wait.try_recv() {
                if EditOutcome::parse(&line).is_ok() {
                    self.answers.push(line);
                }
            }
        }
    }

    fn adopt_document(&mut self, document: DocumentId) {
        self.document = Some(document);
        let durable = self
            .app
            .document_store
            .checkpoint_seq(document)
            .expect("an open document has a checkpoint");
        self.base = durable.0;
        self.head = durable.0;
        self.durable = durable.0;
    }

    /// Settle, project, and check the step as the chain `actions` from the
    /// previous state, then every invariant on the state reached.
    fn step(&mut self, what: &str, actions: &[&str]) {
        self.settle();
        let next = self.project();
        self.trace.push(format!("{what}: {actions:?} -> {next:?}"));
        assert!(
            chain_admits(&self.model, &self.state, actions, &next),
            "{what}: {:?} -> {next:?} is not the chain {actions:?}\ntrace:\n{}",
            self.state,
            self.trace.join("\n")
        );
        let broken = broken_invariants(&self.model, &next);
        assert!(
            broken.is_empty(),
            "{what}: {next:?} breaks {broken:?}\ntrace:\n{}",
            self.trace.join("\n")
        );
        self.state = next;
    }

    fn request(&self) -> EditRequest {
        EditRequest {
            uri: self.uri.clone(),
            line: None,
            create: false,
            for_word: None,
            wait: true,
        }
    }

    fn call(&mut self) -> EditCall {
        let ticket = crate::control_edit::mint_ticket();
        self.ticket = Some(ticket);
        let (reply, answer) = std::sync::mpsc::channel();
        self.reply = Some(answer);
        super::tests::call(ticket, self.session, self.request(), reply)
    }

    /// `edit`, admitted inline (headless, no event loop).
    fn edit(&mut self) -> String {
        let call = self.call();
        self.app.begin_edit(call);
        let line = self
            .reply
            .as_ref()
            .and_then(|reply| reply.try_recv().ok())
            .expect("an inline edit answers at once");
        self.step("edit", &["Request", "Admit"]);
        line
    }

    /// `edit` with its admission left in flight; returns the outcome its
    /// worker would deliver.
    fn edit_in_flight(&mut self) -> DocumentAdmissionOutcome {
        let call = self.call();
        let (wid, key, ticket) = self
            .app
            .begin_edit_in_flight_for_test(call)
            .expect("the request is admitted to the in-flight phase");
        self.step("edit (admission in flight)", &["Request"]);
        let result = crate::native_document_host::admit_local_file(
            &key.uri,
            crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
            false,
        )
        .map(Box::new)
        .map_err(DocumentOpenFailure);
        DocumentAdmissionOutcome {
            ticket,
            wid,
            key,
            result,
        }
    }

    /// `await edit ticket=…`.
    fn wait(&mut self, actions: &[&str]) -> WaiterId {
        let key = AwaitKey::Ticket(self.ticket.expect("a ticket to wait on"));
        self.wait_on(key, actions)
    }

    fn wait_on(&mut self, key: AwaitKey, actions: &[&str]) -> WaiterId {
        let waiter = WaiterId(self.next_waiter);
        self.next_waiter += 1;
        let (reply, answer) = std::sync::mpsc::channel();
        self.waits.push(answer);
        let real_uri = match &key {
            AwaitKey::Uri(uri) => crate::control_edit::real_file_uri(uri),
            AwaitKey::Ticket(_) => None,
        };
        self.app.await_edit(AwaitCall {
            waiter,
            key,
            real_uri,
            reply,
        });
        self.step("await edit", actions);
        waiter
    }

    fn type_text(&mut self, text: &str) {
        self.app
            .dispatch_native_event(
                self.wid,
                AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
            )
            .unwrap();
        self.step("type", &["Edit"]);
    }

    /// Bring the tab holding `view` to the front (not a model step).
    fn front(&mut self, view: ViewId) {
        let tab = self.app.windows[&self.wid]
            .tab_set
            .tabs()
            .iter()
            .find(|tab| tab.root.contains(view))
            .map(|tab| tab.id)
            .expect("the view's tab");
        super::tests::select(&mut self.app, self.wid, tab);
        self.step("select a tab", &[]);
    }

    /// Close the ticket's view's tab. A close that was requested (it closed, or
    /// it waits for its checkpoint) froze the head as the model's `close_seq`;
    /// a refused one requested nothing.
    fn close_edit_view(&mut self, actions: &[&str]) -> Result<(), String> {
        self.front(self.edit_view.expect("the ticket's view"));
        let head = self.state["head"];
        let closed = self.app.close_active_native_tab(self.wid);
        self.note_close(head, closed.is_ok());
        self.step("close the ticket's view", actions);
        closed
    }

    fn note_close(&mut self, head: i64, closed: bool) {
        let waits = self
            .document
            .is_some_and(|document| self.app.document_close_pending_for_test(document));
        if closed || waits {
            self.close_seq = head;
        }
    }

    /// Split the ticket's tab with a terminal and focus the ticket's view
    /// again (not a model step), so its close is one leaf's.
    fn split_edit_tab(&mut self) {
        let view = self.edit_view.expect("the ticket's view");
        self.front(view);
        self.app
            .split_active_with_stub_terminal(self.wid, crate::tab_model::SplitAxis::Horizontal);
        super::tests::focus_leaf(&mut self.app, self.wid, view);
        self.step("split the ticket's tab", &[]);
    }

    /// Close the ticket's view, one leaf of a split tab.
    fn close_edit_leaf(&mut self, actions: &[&str]) -> Result<(), String> {
        let head = self.state["head"];
        let closed = self.app.close_focused_mixed_leaf(self.wid);
        self.note_close(head, closed.is_ok());
        self.step("close the ticket's leaf", actions);
        closed
    }

    /// The person presses done (C-c C-c) in the ticket's view. The press
    /// froze the head as the model's `finish_seq`, and a close it led to the
    /// head then as `close_seq` (a save made the head no newer).
    fn finish(&mut self, actions: &[&str]) {
        let head = self.state["head"];
        self.finish_seq = head;
        self.press(EditorCommand::FinishEdit);
        self.note_close(head, self.edit_view_gone());
        self.step("done (C-c C-c)", actions);
    }

    /// The person presses abort (C-c C-k) in the ticket's view. Its discard
    /// moved the head on only when the buffer was dirty and held no text the
    /// edit did not type; the close it then asks for freezes the head the
    /// discard left.
    fn abort(&mut self, actions: &[&str]) {
        let dirty = self.state["durable"] < self.state["head"];
        let foreign = self.state["foreign"] == 1;
        self.press(EditorCommand::AbortEdit);
        let view = self.edit_view.expect("the ticket's view");
        let aborting = matches!(
            self.app.native_documents.edits.endings.get(&view),
            Some(Ending::Abort { .. })
        );
        let discarded = self.text_moved();
        if self.edit_view_gone() || aborting || discarded {
            self.abort_writes = self.state["writes"];
        }
        self.note_abort_discard(foreign, discarded);
        let head = self.state["head"] + i64::from(dirty && !foreign);
        self.note_close(head, self.edit_view_gone());
        self.step("abort (C-c C-k)", actions);
    }

    /// Whether the document's head moved since the last step (while the
    /// store still has the document).
    fn text_moved(&self) -> bool {
        let (head, _) = self.numbers();
        head as i64 - self.base as i64 != self.state["head"]
    }

    /// An abort pressed while the document held text the edit did not type
    /// (`foreign`) that then moved the text threw that text away: `lost`.
    fn note_abort_discard(&mut self, foreign: bool, discarded: bool) {
        if foreign && discarded {
            self.lost = 1;
        }
    }

    /// The person opens the file in a tab of their own.
    fn person_opens(&mut self) {
        let uri = self.uri.clone();
        self.app.open_document_tab(AppKind::Editor, &uri).unwrap();
        self.person_view = self.app.active_native_view(self.wid).map(|(_, view)| view);
        self.step("the person opens the file", &["PersonOpens"]);
    }

    /// The person types in their own view of the file.
    fn person_types(&mut self, text: &str) {
        let view = self.person_view.expect("the person's view");
        let tab = super::tests::tab_of(&self.app, self.wid, view);
        super::tests::select(&mut self.app, self.wid, tab);
        super::tests::focus_leaf(&mut self.app, self.wid, view);
        self.app
            .dispatch_native_event(
                self.wid,
                AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
            )
            .unwrap();
        self.step("the person types in their view", &["PersonEdits"]);
    }

    /// The person closes their own view of the file.
    fn person_closes(&mut self) {
        self.front(self.person_view.expect("the person's view"));
        self.app.close_active_native_tab(self.wid).unwrap();
        self.step("the person closes their view", &["PersonCloses"]);
    }

    /// The caller's tab in front (not a model step), so an `edit` opens in
    /// front of the person.
    fn caller_in_front(&mut self) {
        super::tests::select(&mut self.app, self.wid, self.caller_tab);
        self.step("the caller's tab in front", &[]);
    }

    /// Another program writes the file. aterm wrote nothing, so the rig
    /// counts no write.
    fn write_externally(&mut self, bytes: &[u8]) {
        std::fs::write(&self.path, bytes).unwrap();
        self.file = read_file(&self.path);
    }

    /// The document's text now.
    fn text(&self) -> String {
        let document = self.document.expect("the document");
        self.app
            .document_store
            .snapshot(document)
            .expect("the document is open")
            .text
            .as_ref()
            .to_string()
    }

    /// The status line of Editor view `view`.
    fn status_of(&self, view: Option<ViewId>) -> Option<String> {
        match self.app.native_runtime.view_state(view?) {
            Some(crate::native_app::AppViewState::Editor(state)) => state.status.clone(),
            _ => None,
        }
    }

    /// Dispatch `command` to the ticket's view, in front.
    fn press(&mut self, command: EditorCommand) {
        let view = self.edit_view.expect("the ticket's view");
        let tab = super::tests::tab_of(&self.app, self.wid, view);
        super::tests::select(&mut self.app, self.wid, tab);
        super::tests::focus_leaf(&mut self.app, self.wid, view);
        self.app
            .dispatch_native_event(self.wid, AppEvent::EditorCommand(command))
            .unwrap();
    }

    fn edit_view_gone(&self) -> bool {
        self.edit_view
            .is_some_and(|view| self.app.view_store.get(view).is_none())
    }
}

/// The file at `path` as the rig compares it between steps: what tells it
/// from a file renamed over it ([`file_identity`]) and its bytes, or `None`
/// while it does not exist.
fn read_file(path: &Path) -> Option<(Option<u128>, Vec<u8>)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((file_identity(&metadata), std::fs::read(path).ok()?))
}

/// What tells a file from another renamed over it with the same bytes (a
/// save writes a new file and renames it in): its inode.
#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> Option<u128> {
    use std::os::unix::fs::MetadataExt;
    Some(u128::from(metadata.ino()))
}

/// What tells a file from another renamed over it with the same bytes: with
/// no inode to read, its modification time.
#[cfg(not(unix))]
fn file_identity(metadata: &std::fs::Metadata) -> Option<u128> {
    let modified = metadata.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(since.as_nanos())
}

/// Whether the chain of `actions` leads from `prev` to `next` (no actions: a
/// stutter, `next == prev`).
fn chain_admits(model: &Model, prev: &State, actions: &[&str], next: &State) -> bool {
    let mut states = vec![prev.clone()];
    for action in actions {
        states = states
            .iter()
            .flat_map(|state| model.successors(action, state))
            .collect();
    }
    states.contains(next)
}

fn broken_invariants(model: &Model, state: &State) -> BTreeSet<&'static str> {
    model
        .invariants
        .iter()
        .filter(|invariant| !model.check_invariant(invariant.name, state))
        .map(|invariant| invariant.name)
        .collect()
}

/// A dirty leaf's close waits in the close protocol for its save; the outcome
/// comes once the save is durable, and the waiter hears it then.
fn dirty_close_after_its_ack() {
    let mut rig = Rig::new("dirty");
    let admission = rig.edit_in_flight();
    rig.app.complete_document_admission(admission).unwrap();
    rig.step("the admission lands", &["Admit"]);
    rig.wait(&["Await"]);
    rig.split_edit_tab();
    rig.type_text("one ");
    let document = rig.document.unwrap();
    let held = rig.app.hold_document_save_for_test(document);
    rig.step("a save starts", &[]);
    assert!(rig.close_edit_leaf(&["RequestClose"]).is_err());
    assert_eq!(
        rig.state["closing"], 1,
        "the close waits for its checkpoint"
    );
    assert_eq!(rig.state["answered"], 0);
    let view = rig.edit_view.unwrap();
    rig.app
        .land_held_document_save_for_test(document, view, held)
        .unwrap();
    rig.step(
        "the save lands and the close completes",
        &["SaveAck", "Retire"],
    );
    assert_eq!(
        (rig.state["answered"], rig.state["saved"]),
        (1, 1),
        "answered saved=1 once the checkpoint covers the close"
    );
    rig.wait(&[]);
    assert_eq!(rig.answers.len(), 2, "a late waiter reads the ring");
}

/// A dirty whole tab's close is refused while its save is in flight (nothing
/// is requested, nothing answers); once the save lands, closing it again
/// answers `saved=1`.
fn refused_whole_tab_close_then_closed() {
    let mut rig = Rig::new("whole-tab");
    rig.edit();
    rig.wait(&["Await"]);
    rig.type_text("four ");
    let document = rig.document.unwrap();
    let held = rig.app.hold_document_save_for_test(document);
    rig.step("a save starts", &[]);
    assert!(rig.close_edit_view(&[]).is_err(), "refused while saving");
    let view = rig.edit_view.unwrap();
    rig.app
        .land_held_document_save_for_test(document, view, held)
        .unwrap();
    rig.step("the save lands", &["SaveAck"]);
    assert_eq!(rig.state["answered"], 0, "the view is still open");
    rig.close_edit_view(&["RequestClose", "Retire"]).unwrap();
    assert_eq!((rig.state["answered"], rig.state["saved"]), (1, 1));
}

/// The waiter leaves, the ticket is orphaned after five seconds and keeps its
/// view, a waiter comes back and adopts it, and a clean close answers.
fn orphan_and_adopt() {
    let mut rig = Rig::new("orphan");
    rig.edit();
    let waiter = rig.wait(&["Await"]);
    let now = Instant::now();
    rig.app.withdraw_edit_waiter_at(waiter, now);
    rig.step("the waiter hangs up", &["Withdraw"]);
    rig.app.sweep_orphaned_edits(now);
    rig.step("swept within the grace", &[]);
    rig.app.sweep_orphaned_edits(now + ORPHAN_AFTER);
    rig.step("swept past the grace", &["Orphan"]);
    assert_eq!((rig.state["state"], rig.state["view"]), (3, 1));
    rig.wait(&["Await"]);
    rig.close_edit_view(&["RequestClose", "Retire"]).unwrap();
    assert_eq!(
        (rig.state["answered"], rig.state["saved"]),
        (1, 0),
        "nothing was saved"
    );
}

/// The person's own view of the file closes without answering the ticket
/// (`Buggy = 4` would); the ticket's final dirty close saves and answers.
fn the_persons_view_never_answers() {
    let mut rig = Rig::new("person");
    let uri = rig.uri.clone();
    rig.app.open_document_tab(AppKind::Editor, &uri).unwrap();
    rig.person_view = rig.app.active_native_view(rig.wid).map(|(_, view)| view);
    rig.step("the person opens the file", &["PersonOpens"]);
    super::tests::select(&mut rig.app, rig.wid, rig.caller_tab);
    rig.step("the caller's tab in front", &[]);
    rig.edit();
    assert_ne!(rig.edit_view, rig.person_view, "a fresh view");
    rig.wait(&["Await"]);
    rig.type_text("two ");

    let before = rig.state.clone();
    rig.front(rig.person_view.unwrap());
    rig.app.close_active_native_tab(rig.wid).unwrap();
    rig.step("the person closes their view", &["PersonCloses"]);
    assert_eq!(rig.state["answered"], 0);
    let buggy = interp::with_buggy(&rig.model, 4);
    let answered_early = buggy.successors("PersonCloses", &before);
    assert!(!answered_early.contains(&rig.state));
    assert!(
        answered_early
            .iter()
            .all(|state| !buggy.check_invariant("WaiterAnsweredOnlyAfterViewGone", state)),
        "Buggy = 4 answers the ticket while its view is open"
    );

    rig.close_edit_view(&["RequestClose", "SaveAck", "Retire"])
        .unwrap();
    assert_eq!((rig.state["answered"], rig.state["saved"]), (1, 1));
}

/// Refusals and cancels: a missing file refuses at admission, a cancelled
/// in-flight admission is dropped when it lands, and nothing is answered.
fn refuse_and_cancel() {
    let mut rig = Rig::new("refuse");
    rig.uri.push_str(".gone");
    let call = rig.call();
    rig.app.begin_edit(call);
    rig.step("edit of a missing file", &["Request", "Refuse"]);

    let mut rig = Rig::new("cancel");
    let admission = rig.edit_in_flight();
    rig.app.cancel_edit(rig.ticket.unwrap());
    rig.step("the edit times out", &["Refuse"]);
    assert!(rig.app.complete_document_admission(admission).is_err());
    rig.step("the late admission is dropped", &[]);
    assert_eq!(rig.state["view"], 0);
}

/// The `edit` gives up just as its view opens (the cancel-after-admit race:
/// its deadline or a hangup, with the install already on the main thread).
fn cancel_after_the_view_opened() {
    let mut rig = Rig::new("cancel-open");
    let admission = rig.edit_in_flight();
    rig.app.complete_document_admission(admission).unwrap();
    rig.step("the admission lands", &["Admit"]);
    rig.app.cancel_edit(rig.ticket.unwrap());
    rig.step("the edit gave up", &["Orphan"]);
    assert_eq!((rig.state["state"], rig.state["view"]), (3, 1));

    let mut rig = Rig::new("cancel-held");
    let admission = rig.edit_in_flight();
    rig.app.complete_document_admission(admission).unwrap();
    rig.step("the admission lands", &["Admit"]);
    let uri = rig.uri.clone();
    rig.wait_on(AwaitKey::Uri(uri), &["Await"]);
    rig.app.cancel_edit(rig.ticket.unwrap());
    rig.step("the edit gave up", &[]);
    assert_eq!((rig.state["state"], rig.state["waiter"]), (2, 1));
    rig.close_edit_view(&["RequestClose", "Retire"]).unwrap();
    assert_eq!(rig.state["answered"], 1);
}

/// NEGATIVE CONTROL: the dirty close's run, with the ticket resolved while its
/// close still waits for `checkpoint_ack`.
fn answer_before_ack_is_caught() {
    let mut rig = Rig::new("control");
    rig.edit();
    rig.wait(&["Await"]);
    rig.split_edit_tab();
    rig.type_text("three ");
    let document = rig.document.unwrap();
    let _held = rig.app.hold_document_save_for_test(document);
    rig.step("a save starts", &[]);
    assert!(rig.close_edit_leaf(&["RequestClose"]).is_err());
    let before = rig.state.clone();

    let buggy = interp::with_buggy(&rig.model, 1);
    let early = buggy.successors("AnswerAtRequest", &before);
    assert!(!early.is_empty(), "Buggy = 1 can answer here");
    for state in &early {
        assert!(!rig.model.check_invariant("SavedOnlyWhenAcked", state));
        assert!(
            !rig.model
                .check_invariant("WaiterAnsweredOnlyAfterViewGone", state)
        );
    }

    rig.app.resolve_edit(rig.ticket.unwrap(), None);
    rig.settle();
    let rigged = rig.project();
    assert_eq!(rigged["answered"], 1, "the rigged run answered the waiter");
    assert!(
        interp::admits(&rig.model, &before, &rigged).is_none(),
        "the model admits no step to {rigged:?}"
    );
    assert!(
        broken_invariants(&rig.model, &rigged).contains("WaiterAnsweredOnlyAfterViewGone"),
        "{rigged:?} must break WaiterAnsweredOnlyAfterViewGone"
    );
}

/// Done on a dirty buffer whose earlier save is still in flight: the view is
/// finishing (its save queued behind the one in flight) and nothing answers;
/// once the saves land, the view closes and the ticket answers `finished
/// saved=1`. Done on a clean buffer closes at once: `finished saved=0`.
fn finish_waits_for_its_save() {
    let mut rig = Rig::new("finish");
    rig.edit();
    rig.wait(&["Await"]);
    rig.type_text("five ");
    let document = rig.document.unwrap();
    let held = rig.app.hold_document_save_for_test(document);
    rig.step("a save starts", &[]);
    rig.finish(&["Finish"]);
    assert_eq!(
        (
            rig.state["finishing"],
            rig.state["view"],
            rig.state["answered"]
        ),
        (1, 1, 0),
        "finishing: the view waits for its save"
    );
    let view = rig.edit_view.unwrap();
    rig.app
        .land_held_document_save_for_test(document, view, held)
        .unwrap();
    rig.close_seq = rig.state["head"];
    rig.step(
        "the save lands and the view closes",
        &["SaveAck", "RequestClose", "Retire"],
    );
    assert_eq!(
        (
            rig.state["result"],
            rig.state["saved"],
            rig.state["answered"]
        ),
        (2, 1, 1),
        "finished saved=1, once the checkpoint covers the done"
    );
    assert_eq!(std::fs::read_to_string(&rig.path).unwrap(), "five base\n");

    let mut rig = Rig::new("finish-clean");
    rig.edit();
    rig.wait(&["Await"]);
    rig.finish(&["Finish", "RequestClose", "Retire"]);
    assert_eq!(
        (rig.state["result"], rig.state["saved"], rig.state["writes"]),
        (2, 0, 0)
    );

    let mut rig = Rig::new("finish-shared");
    let uri = rig.uri.clone();
    rig.app.open_document_tab(AppKind::Editor, &uri).unwrap();
    rig.person_view = rig.app.active_native_view(rig.wid).map(|(_, view)| view);
    rig.step("the person opens the file", &["PersonOpens"]);
    super::tests::select(&mut rig.app, rig.wid, rig.caller_tab);
    rig.step("the caller's tab in front", &[]);
    rig.edit();
    rig.wait(&["Await"]);
    rig.type_text("six ");
    rig.finish(&["Finish", "SaveAck", "RequestClose", "Retire"]);
    assert_eq!(
        (rig.state["result"], rig.state["saved"], rig.state["other"]),
        (2, 1, 1)
    );
    assert_eq!(std::fs::read_to_string(&rig.path).unwrap(), "six base\n");
}

/// Abort discards the buffer's changes, writing nothing, and closes: the
/// ticket answers `aborted saved=0` and the file is as the last save left
/// it.
fn abort_discards_and_writes_nothing() {
    let mut rig = Rig::new("abort");
    rig.edit();
    rig.wait(&["Await"]);
    rig.type_text("seven ");
    rig.abort(&["Abort", "RequestClose", "Retire"]);
    assert_eq!(
        (
            rig.state["result"],
            rig.state["saved"],
            rig.state["writes"],
            rig.state["answered"]
        ),
        (3, 0, 0, 1)
    );
    assert_eq!(std::fs::read_to_string(&rig.path).unwrap(), "base\n");

    let mut rig = Rig::new("abort-after-save");
    rig.edit();
    rig.wait(&["Await"]);
    rig.type_text("eight ");
    rig.app
        .dispatch_native_event(rig.wid, AppEvent::EditorCommand(EditorCommand::Save))
        .unwrap();
    rig.step("C-x C-s", &["SaveAck"]);
    assert_eq!(rig.state["writes"], 1);
    rig.type_text("nine ");
    rig.abort(&["Abort", "RequestClose", "Retire"]);
    assert_eq!(
        (rig.state["result"], rig.state["saved"], rig.state["writes"]),
        (3, 0, 1)
    );
    assert_eq!(std::fs::read_to_string(&rig.path).unwrap(), "eight base\n");

    let mut rig = Rig::new("abort-orphan");
    rig.edit();
    let waiter = rig.wait(&["Await"]);
    let now = Instant::now();
    rig.app.withdraw_edit_waiter_at(waiter, now);
    rig.step("the waiter hangs up", &["Withdraw"]);
    rig.app.sweep_orphaned_edits(now + ORPHAN_AFTER);
    rig.step("swept past the grace", &["Orphan"]);
    rig.type_text("ten ");
    rig.abort(&[]);
    assert_eq!(
        (rig.state["state"], rig.state["view"], rig.state["aborting"]),
        (3, 1, 0),
        "refused: the view and its text stay"
    );
    rig.finish(&["Finish", "SaveAck", "RequestClose", "Retire"]);
    assert_eq!((rig.state["result"], rig.state["saved"]), (2, 1));
}

/// NEGATIVE CONTROL (`AbortSaves`, `Buggy = 5`): the abort's run, rigged to
/// save the buffer where the implementation discards it.
fn abort_that_saves_is_caught() {
    let mut rig = Rig::new("abort-control");
    rig.edit();
    rig.wait(&["Await"]);
    rig.type_text("eleven ");
    let before = rig.state.clone();
    rig.abort_writes = rig.state["writes"];
    let view = rig.edit_view.unwrap();
    let document = rig.document.unwrap();
    rig.app
        .native_documents
        .edits
        .endings
        .insert(view, Ending::Abort { document });
    rig.app.save_document_checkpoint(document, view).unwrap();
    rig.close_seq = rig.state["head"];
    rig.app.close_editor_view(view).unwrap();
    rig.settle();
    let rigged = rig.project();
    assert_eq!(
        (rigged["result"], rigged["writes"]),
        (3, 1),
        "the rigged run answered aborted after writing the file"
    );
    assert!(
        !chain_admits(
            &rig.model,
            &before,
            &["Abort", "RequestClose", "Retire"],
            &rigged
        ),
        "the model admits no abort to {rigged:?}"
    );
    assert!(
        broken_invariants(&rig.model, &rigged).contains("AbortNeverAdvancesCheckpoint"),
        "{rigged:?} must break AbortNeverAdvancesCheckpoint"
    );
    let buggy = interp::with_buggy(&rig.model, 5);
    assert!(chain_admits(
        &buggy,
        &before,
        &["Abort", "RequestClose", "Retire"],
        &rigged
    ));
}

/// Done whose save does not land stops finishing (`FinishHalts`). Done is
/// pressed behind a save in flight, so its own save queues; an abort pressed
/// then is refused and leaves the done finishing.
fn finish_halts_when_its_save_does_not_land() {
    let mut rig = Rig::new("finish-halts");
    rig.edit();
    rig.wait(&["Await"]);
    rig.type_text("twelve ");
    let document = rig.document.unwrap();
    let held = rig.app.hold_document_save_for_test(document);
    rig.step("a save starts", &[]);
    rig.finish(&["Finish"]);
    rig.abort(&[]);
    assert_eq!(
        (rig.state["finishing"], rig.state["aborting"]),
        (1, 0),
        "an abort refused while the save lands leaves the done finishing"
    );
    rig.write_externally(b"theirs\n");
    let view = rig.edit_view.unwrap();
    assert!(
        rig.app
            .land_held_document_save_for_test(document, view, held)
            .is_err()
    );
    rig.step("the save finds the file changed", &["FinishHalts"]);
    assert_eq!(
        (
            rig.state["finishing"],
            rig.state["view"],
            rig.state["answered"]
        ),
        (0, 1, 0)
    );
    assert_eq!(
        rig.status_of(rig.edit_view),
        Some(format!(
            "Not done: {}",
            crate::native_app::DISK_CONFLICT_RECOVERY
        ))
    );
    rig.finish(&["Finish", "FinishHalts"]);
    assert!(
        rig.status_of(rig.edit_view)
            .is_some_and(|status| status.starts_with("Not done: Save blocked")),
        "{:?}",
        rig.status_of(rig.edit_view)
    );
    rig.abort(&["Abort", "RequestClose", "Retire"]);
    assert_eq!(
        (rig.state["result"], rig.state["saved"], rig.state["writes"]),
        (3, 0, 0)
    );
    assert_eq!(std::fs::read_to_string(&rig.path).unwrap(), "theirs\n");
}

/// Abort throws away only what the edit typed. With the person's own view
/// of the file clean, the discard reverts that view too.
fn abort_keeps_text_it_did_not_type() {
    let mut rig = Rig::new("abort-shared-clean");
    rig.person_opens();
    rig.caller_in_front();
    rig.edit();
    rig.wait(&["Await"]);
    rig.type_text("theirs ");
    rig.abort(&["Abort", "RequestClose", "Retire"]);
    assert_eq!(
        (rig.state["result"], rig.state["other"], rig.state["lost"]),
        (3, 1, 0)
    );
    assert_eq!(rig.text(), "base\n", "only the edit's typing was there");

    let mut rig = Rig::new("abort-dirty-at-open");
    rig.person_opens();
    rig.person_types("mine ");
    rig.caller_in_front();
    rig.edit();
    assert_eq!(rig.state["foreign"], 1, "dirty when the edit opened");
    rig.wait(&["Await"]);
    rig.type_text("theirs ");
    rig.abort(&["Abort", "RequestClose", "Retire"]);
    assert_eq!(
        (
            rig.state["result"],
            rig.state["saved"],
            rig.state["writes"],
            rig.state["other"]
        ),
        (3, 0, 0, 1)
    );
    let kept = rig.text();
    assert!(
        kept.contains("mine ") && kept.contains("theirs "),
        "{kept:?}"
    );
    assert_eq!(std::fs::read_to_string(&rig.path).unwrap(), "base\n");
    assert_eq!(
        rig.status_of(rig.person_view).as_deref(),
        Some(super::ABORT_KEPT)
    );

    let mut rig = Rig::new("abort-typed-beside");
    rig.person_opens();
    rig.caller_in_front();
    rig.edit();
    rig.wait(&["Await"]);
    rig.type_text("theirs ");
    rig.person_types("mine ");
    assert_eq!(rig.state["foreign"], 1, "typed in the person's view");
    rig.abort(&["Abort", "RequestClose", "Retire"]);
    assert_eq!((rig.state["result"], rig.state["saved"]), (3, 0));
    assert!(rig.text().contains("mine "));

    let mut rig = Rig::new("abort-foreign-alone");
    rig.person_opens();
    rig.person_types("mine ");
    rig.caller_in_front();
    rig.edit();
    rig.wait(&["Await"]);
    rig.person_closes();
    rig.abort(&[]);
    assert_eq!(
        rig.status_of(rig.edit_view).as_deref(),
        Some(super::ABORT_FOREIGN)
    );
    assert_eq!((rig.state["view"], rig.state["aborting"]), (1, 0));
    assert!(rig.text().contains("mine "));
    rig.finish(&["Finish", "SaveAck", "RequestClose", "Retire"]);
    assert_eq!((rig.state["result"], rig.state["saved"]), (2, 1));
    assert_eq!(std::fs::read_to_string(&rig.path).unwrap(), "mine base\n");
}

/// NEGATIVE CONTROL (`AbortDiscardsOthersText`, `Buggy = 7`): the abort's
/// run with the person's unsaved text in their own view, rigged to discard
/// the shared document anyway.
fn abort_that_discards_others_text_is_caught() {
    let mut rig = Rig::new("abort-others-control");
    rig.person_opens();
    rig.person_types("mine ");
    rig.caller_in_front();
    rig.edit();
    rig.wait(&["Await"]);
    let before = rig.state.clone();
    rig.abort_writes = rig.state["writes"];
    let view = rig.edit_view.unwrap();
    let document = rig.document.unwrap();
    rig.app
        .native_documents
        .edits
        .endings
        .insert(view, Ending::Abort { document });
    rig.app.discard_document_changes(document, view).unwrap();
    let discarded = rig.text_moved();
    rig.note_abort_discard(before["foreign"] == 1, discarded);
    rig.close_seq = before["head"] + 1;
    rig.app.close_editor_view(view).unwrap();
    rig.settle();
    let rigged = rig.project();
    assert_eq!(
        (rigged["result"], rigged["lost"]),
        (3, 1),
        "the rigged run answered aborted after throwing the person's text away"
    );
    assert_eq!(rig.text(), "base\n");
    assert!(
        !chain_admits(
            &rig.model,
            &before,
            &["Abort", "RequestClose", "Retire"],
            &rigged
        ),
        "the model admits no abort to {rigged:?}"
    );
    assert!(
        broken_invariants(&rig.model, &rigged).contains("AbortKeepsOthersText"),
        "{rigged:?} must break AbortKeepsOthersText"
    );
    let buggy = interp::with_buggy(&rig.model, 7);
    assert!(chain_admits(
        &buggy,
        &before,
        &["Abort", "RequestClose", "Retire"],
        &rigged
    ));
}

#[test]
fn refines_amacs_edit_ticket() {
    dirty_close_after_its_ack();
    refused_whole_tab_close_then_closed();
    orphan_and_adopt();
    the_persons_view_never_answers();
    refuse_and_cancel();
    cancel_after_the_view_opened();
    finish_waits_for_its_save();
    abort_discards_and_writes_nothing();
    finish_halts_when_its_save_does_not_land();
    abort_keeps_text_it_did_not_type();
    answer_before_ack_is_caught();
    abort_that_saves_is_caught();
    abort_that_discards_others_text_is_caught();
}
