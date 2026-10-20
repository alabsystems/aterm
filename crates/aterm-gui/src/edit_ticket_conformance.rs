// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `edit` tickets (`app_edit`) to
//! `aterm_models_native::models_amacs_ticket::amacs_edit_ticket_model`
//! (`AmacsEditTicket`, amacs design §4.9).
//!
//! A headless `App` serving `edit` is driven through the shipping seams: the
//! request and its two-phase admission (`begin_edit`, or the in-flight seam
//! and `complete_document_admission`), `await edit` and its withdrawal, the
//! lazy orphan sweep, typing (`dispatch_native_event`), saves (a held save
//! landed through `finish_native_document_save`), closes of a whole tab
//! (`close_active_native_tab`, refused while a dirty view's save is in flight)
//! and of one leaf of a split tab (`close_focused_mixed_leaf`, which waits in
//! the close protocol for `checkpoint_ack`), both reaching
//! `remove_view_link`, the person's own open of the file and `cancel_edit`. After each step [`Rig::project`] reads the real state onto
//! the model's variables and the step is checked as the named chain of model
//! actions (a seam that does several things at once, such as a save landing
//! and the close it held then retiring the view, is a chain), and every
//! invariant is evaluated on the state reached.
//!
//! Projection: `state` from the ticket's place in `EditTickets` (admitting,
//! open, orphaned, in the resolved ring, or gone without an outcome);
//! `view`/`other` from whether the ticket's view and the person's own view are
//! still linked; `head`/`durable` from the document store's sequence and
//! durable checkpoint, relative to the checkpoint the document opened with;
//! `start` from the ticket's open phase; `closing` from the close protocol's
//! pending close (a refused whole-tab close requests nothing); `waiter` from the parked waiters; `outcomes` from the ring;
//! `answered` from what the rig's own `await edit` channels received; and
//! `saved`/`answer_seq` from the recorded outcome. `close_seq` is the head the
//! rig saw when it asked for the close (the model's `RequestClose` sets it to
//! the head, deterministically). Once the store no longer has the document,
//! the last numbers read stand.
//!
//! NEGATIVE CONTROL: the same real run with the ticket resolved while its
//! dirty close still waits for `checkpoint_ack` (the shape the model's
//! `AnswerAtRequest`, `Buggy = 1`, allows) projects to a step the model
//! rejects and a state that breaks `WaiterAnsweredOnlyAfterViewGone`; the
//! model's own answer-before-ack successor breaks `SavedOnlyWhenAcked` too.

use std::collections::BTreeSet;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use amacs::protocol::edit::{AwaitKey, EditOutcome, EditRequest, Ticket};
use aterm_models_native::models_amacs_ticket::amacs_edit_ticket_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use super::{ORPHAN_AFTER, Phase};
use crate::app_documents::{DocumentAdmissionOutcome, DocumentOpenFailure};
use crate::control_edit::{AwaitCall, EditCall, WaiterId};
use crate::document_store::DocumentId;
use crate::native_app::{AppEvent, AppKind, TextInputEvent};
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
            ticket: None,
            edit_view: None,
            person_view: None,
            document: None,
            base: 0,
            head: 0,
            durable: 0,
            start: 0,
            close_seq: 0,
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

    /// The real state, as the model's variables (see the module doc).
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
        let (head, durable) = self.numbers();
        let closing = self.document.is_some_and(|document| {
            linked(self.edit_view) == 1 && self.app.document_close_pending_for_test(document)
        });
        let relative = |seq: u64| seq as i64 - self.base as i64;
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
/// comes once the save is durable, and the waiter hears it then. Also the
/// in-flight admission and the late waiter that reads the ring.
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
    // The clock is the rig's: the withdrawal starts it at `now`, so no wall
    // time between these steps can orphan the ticket early.
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
    // The control: an implementation that answered here is `Buggy = 4`, whose
    // successor breaks the law the real step kept.
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
/// Nobody learned the ticket, so it is orphaned at once and keeps its view; a
/// waiter that found it first by `uri=` holds it, and the cancel changes
/// nothing.
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

    // The model's own answer-before-ack successor breaks both laws.
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

    // The rigged implementation: resolve now, before the save lands.
    rig.app.resolve_edit(
        rig.ticket.unwrap(),
        amacs::protocol::edit::EditResult::Closed,
    );
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

#[test]
fn refines_amacs_edit_ticket() {
    dirty_close_after_its_ack();
    refused_whole_tab_close_then_closed();
    orphan_and_adopt();
    the_persons_view_never_answers();
    refuse_and_cancel();
    cancel_after_the_view_opened();
    answer_before_ack_is_caught();
}
