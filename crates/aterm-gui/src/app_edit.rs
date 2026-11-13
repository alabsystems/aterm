// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Main-thread `edit` ticket lifecycle.

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use amacs::protocol::edit::{AwaitKey, EditOk, EditOutcome, EditResult, Ticket};
use aterm_buffer::Seq;

use crate::app_documents::{DocumentAdmissionKey, DocumentOpenFailure, LineReveal};
use crate::control_edit::{AwaitCall, EditCall, WaiterId, hold};
use crate::document_store::DocumentId;
use crate::native_app::AppKind;
use crate::native_document_host::AdmittedDocument;
use crate::tab_model::{TabId, ViewId};
use crate::{App, WindowId};

/// Identity of an `edit` ticket ([`Ticket`]).
pub(crate) type EditTicketId = Ticket;

/// Tab placement policy for an admitted Editor document view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// Focuses an existing view of the document or opens a new tab at the end of the strip.
    FocusOrNewTab,
    /// Opens a fresh view in a new tab immediately to the right of the caller's tab.
    NewTabAfter { ticket: EditTicketId },
    /// Opens a fresh view in a new tab at the end of the strip, even beside an open view.
    NewTab,
}

/// Maximum number of resolved ticket outcomes retained for late waiters.
pub(crate) const RESOLVED_RING: usize = 64;

/// Grace period (`5s`) before an unwaited ticket is marked orphaned.
pub(crate) const ORPHAN_AFTER: Duration = Duration::from_secs(5);

/// One open ticket's stage.
#[derive(Debug)]
enum Phase {
    /// The admission is in flight; `reply` answers the `edit` request.
    Admitting { reply: Sender<String> },
    /// The view is installed.
    Open {
        view: ViewId,
        tab: TabId,
        document: DocumentId,
        canonical_uri: String,
        /// The document's durable checkpoint when the view opened: a save
        /// counts for `saved=1` only once the checkpoint passes it.
        start: aterm_buffer::Seq,
    },
}

/// A ticket from its request until it resolves.
#[derive(Debug)]
struct OpenTicket {
    session: u64,
    window: WindowId,
    /// The caller's tab, the new tab's left neighbour.
    caller_tab: TabId,
    /// The file as requested, in the codec's canonical spelling.
    uri: String,
    /// The file's realpath spelling, when the request's lane resolved it
    /// (`control_edit::real_file_uri`): what the admission's grant will name.
    real_uri: Option<String>,
    line: Option<(u32, Option<u32>)>,
    /// `create=1`: a file that does not exist yet admits as an empty text,
    /// and its first save creates it.
    create: bool,
    /// The program the edit is for (`for=`), which the header row names.
    for_word: Option<String>,
    /// A program waits on the answer, or will (its `await edit` is on its
    /// way): false for an `edit … wait=0` until a waiter adopts it.
    awaited: bool,
    /// `ro=1`: the document opens read-only, a fresh buffer nothing can change or save
    /// (`amacs::store::DocumentStore::set_read_only`).
    read_only: bool,
    /// `secret=`: the private grid's columns, by header, whose cells no reader of the window but
    /// the person's eyes gets (`amacs::store::Concealed`); `None` when the request named none.
    secret: Option<Vec<String>>,
    phase: Phase,
    /// Since when no waiter has been parked on it: the orphan clock.
    unwaited_since: Option<Instant>,
    /// The clock ran out ([`App::sweep_orphaned_edits`]).
    orphaned: bool,
    /// The newest sequence of the document that holds text this edit did
    /// not type: its head when the view opened on unsaved changes, or a
    /// commit made in another view of it since
    /// ([`App::edit_document_committed`]).
    foreign_at: Option<Seq>,
    /// The view opened behind the tab the person is on: its tab carries the
    /// attention mark until the person brings it to the front.
    attention: bool,
}

impl OpenTicket {
    fn names(&self, uri: &str) -> bool {
        self.uri == uri
            || self.real_uri.as_deref() == Some(uri)
            || matches!(&self.phase, Phase::Open { canonical_uri, .. } if canonical_uri == uri)
    }

    const fn view(&self) -> Option<ViewId> {
        match self.phase {
            Phase::Open { view, .. } => Some(view),
            Phase::Admitting { .. } => None,
        }
    }

    const fn client_waits(&self) -> bool {
        self.awaited && !self.orphaned
    }

    fn holds_foreign_text(&self, durable: Option<Seq>) -> bool {
        self.foreign_at
            .is_some_and(|seq| durable.is_none_or(|durable| seq > durable))
    }

    fn header(&self) -> crate::native_app::EditHeader {
        crate::native_app::EditHeader {
            for_word: self.for_word.clone(),
            read_only: self.read_only,
            waiter: if !self.awaited {
                crate::native_app::EditWaiter::Never
            } else if self.orphaned {
                crate::native_app::EditWaiter::Gone
            } else {
                crate::native_app::EditWaiter::Waiting
            },
        }
    }
}

/// What the person asked of an Editor view with done or abort, kept from the
/// press until the view retires ([`App::edit_view_retired`]), which reads it
/// for the outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// Done: the Finishing state. The document was at `seq` when the person
    /// pressed it, a save of it was started (none was needed when its
    /// checkpoint `from` already covered `seq`), and the view closes once the
    /// checkpoint covers `seq` ([`App::edit_document_saved`]).
    Finish {
        document: DocumentId,
        seq: Seq,
        from: Seq,
    },
    /// Abort: the document's changes were discarded (nothing was written),
    /// and the view is closing.
    Abort { document: DocumentId },
}

impl Ending {
    const fn document(self) -> DocumentId {
        match self {
            Self::Finish { document, .. } | Self::Abort { document } => document,
        }
    }

    fn result(self, durable: Seq) -> EditResult {
        match self {
            Self::Abort { .. } => EditResult::Aborted,
            Self::Finish { seq, .. } if durable >= seq => EditResult::Finished,
            Self::Finish { .. } => EditResult::Closed,
        }
    }
}

/// Status message shown when `finish-edit` or `abort-edit` is invoked with no active waiter.
pub(crate) const NO_EDIT_WAITING: &str = "nothing is waiting on this file; C-x C-s saves";

const ABORT_GONE: &str = "The program waiting on this edit has gone, so nothing was discarded; \
                          C-c C-c saves and closes";

const ABORT_UNWAITED: &str =
    "Nothing waits on this edit, so nothing was discarded; C-c C-c saves and closes";

const ABORT_SAVING: &str =
    "A save of this file is still landing, so nothing was discarded; C-c C-k again once it lands";

const ABORT_FOREIGN: &str = "This buffer holds unsaved changes made outside this edit, so nothing \
                             was discarded; C-c C-c saves and closes";

const ABORT_KEPT: &str = "The edit was aborted; this buffer keeps its unsaved changes";

/// A ticket's one outcome, kept in the ring.
#[derive(Debug)]
struct Resolved {
    ticket: Ticket,
    uri: String,
    canonical_uri: String,
    outcome: EditOutcome,
}

/// A parked `await edit`.
#[derive(Debug)]
struct Waiter {
    key: AwaitKey,
    /// A `uri=` key's spellings of the file: the codec's canonical one and,
    /// when the lane resolved it, the realpath one.
    spellings: Vec<String>,
    reply: Sender<String>,
    /// A `uri=` wait this process has no ticket for (its server was replaced):
    /// done when no Editor view of the file remains.
    by_views: bool,
}

impl Waiter {
    fn covers(&self, ticket: Ticket, entry: &OpenTicket) -> bool {
        match &self.key {
            AwaitKey::Ticket(t) => *t == ticket,
            AwaitKey::Uri(_) => !self.by_views && self.spellings.iter().any(|uri| entry.names(uri)),
        }
    }

    fn covered_by_any(&self, open: &BTreeMap<Ticket, OpenTicket>) -> bool {
        open.iter()
            .any(|(ticket, entry)| self.covers(*ticket, entry))
    }
}

enum Uncovered {
    /// This line, now.
    Answer(String),
    /// Nothing yet: it waits until no Editor view of its file remains.
    ByViews,
}

/// Registry of active and recently resolved `edit` tickets for this process.
#[derive(Debug, Default)]
pub(crate) struct EditTickets {
    open: BTreeMap<Ticket, OpenTicket>,
    resolved: VecDeque<Resolved>,
    waiters: BTreeMap<WaiterId, Waiter>,
    /// Editor views the person pressed done or abort in, until they retire.
    endings: BTreeMap<ViewId, Ending>,
    /// The `edit` view a done or abort is closing while the person is not
    /// looking at it ([`App::close_edit_view_in_place`]): its ticket does not
    /// hand the window to the caller's tab.
    closing_behind: Option<ViewId>,
    /// The window a whole-tab close is retiring leaves in
    /// ([`App::close_active_native_tab_keeping`]): an `edit` view's ticket hands
    /// that window, and no other, to the caller's tab. A window's own close and
    /// a restore's discard hand none over.
    retiring_in: Option<WindowId>,
    /// Test seam: serve `edit` on the headless test App, which production
    /// refuses (`ERR unsupported headless`).
    #[cfg(test)]
    serve_headless: bool,
}

impl EditTickets {
    const fn serves_headless(&self) -> bool {
        #[cfg(test)]
        {
            self.serve_headless
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn waited(&self, ticket: Ticket) -> bool {
        let Some(entry) = self.open.get(&ticket) else {
            return false;
        };
        self.waiters
            .values()
            .any(|waiter| waiter.covers(ticket, entry))
    }

    /// Whether `ticket` asked for its document read-only (`edit … ro=1`).
    pub(crate) fn read_only(&self, ticket: Ticket) -> bool {
        self.open.get(&ticket).is_some_and(|entry| entry.read_only)
    }

    /// The grid columns `ticket` named concealed (`edit … secret=`), by header; `None` when it
    /// named none or there is no such ticket.
    pub(crate) fn secret(&self, ticket: Ticket) -> Option<&[String]> {
        self.open.get(&ticket)?.secret.as_deref()
    }

    fn ticket_of_view(&self, view: ViewId) -> Option<Ticket> {
        self.open
            .iter()
            .find(|(_, entry)| entry.view() == Some(view))
            .map(|(ticket, _)| *ticket)
    }

    fn remember(&mut self, resolved: Resolved) {
        if self.resolved.len() == RESOLVED_RING {
            self.resolved.pop_front();
        }
        self.resolved.push_back(resolved);
    }

    fn outcome_of(&self, ticket: Ticket) -> Option<EditOutcome> {
        self.resolved
            .iter()
            .rev()
            .find(|resolved| resolved.ticket == ticket)
            .map(|resolved| resolved.outcome)
    }

    fn latest_outcome_for(&self, spellings: &[String]) -> Option<EditOutcome> {
        self.resolved
            .iter()
            .rev()
            .find(|resolved| {
                spellings
                    .iter()
                    .any(|uri| resolved.uri == *uri || resolved.canonical_uri == *uri)
            })
            .map(|resolved| resolved.outcome)
    }
}

fn canonical_spelling(uri: &str) -> String {
    amacs::protocol::uri::file_uri_to_path(uri)
        .and_then(|path| amacs::protocol::uri::path_to_file_uri(&path))
        .unwrap_or_else(|_| uri.to_string())
}

fn outcome_line(outcome: EditOutcome) -> String {
    format!("{}\n", outcome.format())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl App {
    /// Starts admitting an `edit` request (`Wake::EditRequest`).
    pub(crate) fn begin_edit(&mut self, call: EditCall) {
        let ticket = call.ticket;
        let Some((window, uri)) = self.record_edit_ticket(call) else {
            return;
        };
        if let Err(why) = self.request_edit_admission(window, &uri, ticket)
            && let Some(OpenTicket {
                phase: Phase::Admitting { reply },
                ..
            }) = self.native_documents.edits.open.remove(&ticket)
        {
            let _ = reply.send(format!("ERR {}\n", one_line(&why)));
            self.settle_uncovered_edit_waiters();
        }
    }

    /// Where an `edit` from `session` opens: the window holding the session and
    /// the tab hosting it, or the line that refuses it before any file is read
    /// (`ERR unsupported headless`, `ERR no-window`).
    fn edit_host(&self, session: u64) -> Result<(WindowId, TabId), &'static str> {
        if self.headless && !self.native_documents.edits.serves_headless() {
            return Err("ERR unsupported headless\n");
        }
        let window = self.window_of_session(session).ok_or("ERR no-window\n")?;
        let tab = self
            .tab_hosting_session(window, session)
            .ok_or("ERR no-window\n")?;
        Ok((window, tab))
    }

    /// `status editor=`: `amacs` while an `edit` from `session` (the `amacs`
    /// command run in its tab) opens an Editor tab beside it, `-` while
    /// [`Self::edit_host`] refuses it.
    pub(crate) fn editor_word(&self, session: u64) -> &'static str {
        if self.edit_host(session).is_ok() {
            "amacs"
        } else {
            "-"
        }
    }

    /// [`Self::begin_edit`]'s checks: answer a refusal and return `None`, or
    /// record the ticket as admitting and return its window and the file's
    /// canonical spelling.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Request",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    fn record_edit_ticket(&mut self, call: EditCall) -> Option<(WindowId, String)> {
        self.sweep_orphaned_edits(Instant::now());
        let EditCall {
            ticket,
            session,
            request,
            real_uri,
            reply,
        } = call;
        let refuse = |line: &str| {
            let _ = reply.send(line.to_string());
            None
        };
        let (window, caller_tab) = match self.edit_host(session) {
            Ok(host) => host,
            Err(line) => return refuse(line),
        };
        if Self::refuse_file_provider_open(&request.uri).is_err() {
            return refuse("ERR refused file-provider\n");
        }
        let uri = canonical_spelling(&request.uri);
        self.native_documents.edits.open.insert(
            ticket,
            OpenTicket {
                session,
                window,
                caller_tab,
                uri: uri.clone(),
                real_uri,
                line: request.line,
                create: request.create,
                for_word: request.for_word,
                awaited: request.wait,
                read_only: request.read_only,
                secret: request.secret,
                phase: Phase::Admitting { reply },
                unwaited_since: None,
                orphaned: false,
                foreign_at: None,
                attention: false,
            },
        );
        Some((window, uri))
    }

    /// Test seam: starts an `edit` request while leaving document admission in flight.
    #[cfg(test)]
    pub(crate) fn begin_edit_in_flight_for_test(
        &mut self,
        call: EditCall,
    ) -> Option<(
        WindowId,
        DocumentAdmissionKey,
        crate::app_documents::DocumentAdmissionTicket,
    )> {
        let ticket = call.ticket;
        let (window, uri) = self.record_edit_ticket(call)?;
        let key = DocumentAdmissionKey {
            uri,
            kind: AppKind::Editor,
            edit: Some(ticket),
        };
        let admission = self.mint_document_admission(window, key.clone());
        Some((window, key, admission))
    }

    fn tab_hosting_session(&self, wid: WindowId, session: u64) -> Option<TabId> {
        self.windows
            .get(&wid)?
            .tab_set
            .tabs()
            .iter()
            .find_map(|tab| {
                tab.root
                    .any_leaf(&mut |&view| {
                        self.view_store
                            .get(view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                            == Some(session)
                    })
                    .then_some(tab.id)
            })
    }

    /// Completes document admission for an in-flight `edit` ticket.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Admit",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Refuse",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn finish_edit_admission(
        &mut self,
        wid: WindowId,
        key: DocumentAdmissionKey,
        result: Result<Box<AdmittedDocument>, DocumentOpenFailure>,
    ) -> Result<String, String> {
        let Some(ticket) = key.edit else {
            return Err(format!("{} is no edit admission", key.uri));
        };
        let admitting = self
            .native_documents
            .edits
            .open
            .get(&ticket)
            .is_some_and(|entry| matches!(entry.phase, Phase::Admitting { .. }));
        if !admitting {
            return Err(format!(
                "edit ticket {ticket} was cancelled; its admission of {} is dropped",
                key.uri
            ));
        }
        let refusal = if !self.windows.contains_key(&wid) {
            Some("ERR no-window\n".to_string())
        } else {
            match &result {
                Ok(_) => None,
                Err(failure) => Some(format!("ERR refused {}\n", failure.0.refusal_word())),
            }
        };
        let installed = match (refusal, result) {
            (None, Ok(admitted)) => self.install_admitted_document(
                wid,
                AppKind::Editor,
                &key.uri,
                *admitted,
                Placement::NewTabAfter { ticket },
            ),
            (Some(line), _) => Err(line),
            (None, Err(failure)) => Err(format!("ERR refused {}\n", failure.0.refusal_word())),
        };
        let installed = match installed {
            Ok(installed) => installed,
            Err(why) => {
                let line = if why.starts_with("ERR ") {
                    why
                } else {
                    format!("ERR refused unreadable ({})\n", one_line(&why))
                };
                if let Some(OpenTicket {
                    phase: Phase::Admitting { reply },
                    ..
                }) = self.native_documents.edits.open.remove(&ticket)
                {
                    let _ = reply.send(line.clone());
                }
                self.settle_uncovered_edit_waiters();
                return Err(line.trim_end().to_string());
            }
        };
        let Some(entry) = self.native_documents.edits.open.get_mut(&ticket) else {
            return Err(format!("edit ticket {ticket} disappeared while installing"));
        };
        let reveal = entry.line;
        let session = entry.session;
        let attention = entry.attention;
        let awaited = entry.awaited;
        let start = self
            .document_store
            .checkpoint_seq(installed.document)
            .unwrap_or(aterm_buffer::Seq(0));
        let head = self
            .document_store
            .snapshot(installed.document)
            .map_or(start, |snapshot| snapshot.seq);
        entry.foreign_at = (head > start).then_some(head);
        let phase = std::mem::replace(
            &mut entry.phase,
            Phase::Open {
                view: installed.view,
                tab: installed.tab,
                document: installed.document,
                canonical_uri: installed.canonical_uri.clone(),
                start,
            },
        );
        let Phase::Admitting { reply } = phase else {
            return Err(format!("edit ticket {ticket} was not admitting"));
        };
        if let Some((line, col)) = reveal
            && let Err(why) =
                self.reveal_document_line_in_view(installed.view, LineReveal { line, col })
        {
            aterm_log::warn!("edit ticket {ticket}: could not move the caret: {why}");
        }
        if attention {
            self.refresh_aggregate_tab_presentation(wid, installed.tab);
            self.refresh_window_tabs(wid);
            self.request_native_redraw(wid);
        }
        if awaited {
            hold::set(
                session,
                hold::Hold {
                    ticket,
                    view: installed.view.get(),
                },
            );
        }
        self.start_edit_orphan_clock_if_unwaited(ticket, Instant::now());
        self.sync_edit_header(ticket);
        let ok = EditOk {
            ticket,
            view: installed.view.get(),
            doc: installed.document.get(),
            uri: installed.canonical_uri,
        };
        let line = match ok.format() {
            Ok(line) => format!("{line}\n"),
            Err(error) => format!("ERR {error}\n"),
        };
        let _ = reply.send(line.clone());
        Ok(line.trim_end().to_string())
    }

    /// The tab that stays in front of `wid` while `ticket`'s new view opens behind it, where
    /// `front` was in front: by the askpass rule the view takes the front only while the
    /// person is on its caller's tab in the front window. `None` (the view takes the front)
    /// also when the ticket is gone.
    pub(crate) fn admitted_view_kept_front(
        &self,
        wid: WindowId,
        ticket: EditTicketId,
        front: Option<TabId>,
    ) -> Option<TabId> {
        let caller_tab = self.native_documents.edits.open.get(&ticket)?.caller_tab;
        let takes_front = self.frontmost_window == Some(wid) && front == Some(caller_tab);
        front.filter(|_| !takes_front)
    }

    /// Places `ticket`'s newly created `tab` immediately to the right of its caller tab, marks
    /// it when it opened behind the person, and runs the window's one sync for the open.
    pub(crate) fn place_admitted_view(&mut self, wid: WindowId, tab: TabId, ticket: EditTicketId) {
        let caller_tab = self
            .native_documents
            .edits
            .open
            .get(&ticket)
            .map(|entry| entry.caller_tab);
        if let Some(caller_tab) = caller_tab
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            if let Some(caller) = ws.tab_set.tabs().iter().position(|t| t.id == caller_tab) {
                ws.tab_set.reorder(tab, caller + 1);
            }
            let behind = ws.tab_set.active_id() != Some(tab);
            if let Some(entry) = self.native_documents.edits.open.get_mut(&ticket) {
                entry.attention = behind;
            }
        }
        self.resync_active_or_window(wid);
        self.request_native_redraw(wid);
    }

    /// Cancels an in-flight `edit` request or starts its orphan clock if already open.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Refuse",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Orphan",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn cancel_edit(&mut self, ticket: EditTicketId) {
        let now = Instant::now();
        let edits = &mut self.native_documents.edits;
        let waited = edits.waited(ticket);
        match edits.open.get_mut(&ticket) {
            Some(OpenTicket {
                phase: Phase::Admitting { .. },
                ..
            }) => {
                edits.open.remove(&ticket);
            }
            Some(entry) if !waited => {
                entry.unwaited_since = Some(now.checked_sub(ORPHAN_AFTER).unwrap_or(now));
                entry.orphaned = entry.awaited;
                hold::clear(entry.session, ticket);
            }
            Some(_) | None => {}
        }
        self.sweep_orphaned_edits(now);
        self.sync_edit_header(ticket);
        self.settle_uncovered_edit_waiters();
    }

    /// Answers an `await edit` request immediately or parks the waiter until resolution.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Await",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "AwaitResolved",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn await_edit(&mut self, call: AwaitCall) {
        let now = Instant::now();
        self.sweep_orphaned_edits(now);
        let AwaitCall {
            waiter: id,
            key,
            real_uri,
            reply,
        } = call;
        let spellings = match &key {
            AwaitKey::Uri(uri) => {
                let spelled = canonical_spelling(uri);
                let real = real_uri.filter(|real| *real != spelled);
                std::iter::once(spelled).chain(real).collect()
            }
            AwaitKey::Ticket(_) => Vec::new(),
        };
        let mut waiter = Waiter {
            key,
            spellings,
            reply,
            by_views: false,
        };
        let covered: Vec<Ticket> = self
            .native_documents
            .edits
            .open
            .iter()
            .filter(|(ticket, entry)| waiter.covers(**ticket, entry))
            .map(|(ticket, _)| *ticket)
            .collect();
        if covered.is_empty() {
            match self.uncovered_edit_waiter(&waiter) {
                Uncovered::Answer(line) => {
                    let _ = waiter.reply.send(line);
                    return;
                }
                Uncovered::ByViews => waiter.by_views = true,
            }
        }
        self.native_documents.edits.waiters.insert(id, waiter);
        for ticket in covered {
            let Some(entry) = self.native_documents.edits.open.get_mut(&ticket) else {
                continue;
            };
            entry.unwaited_since = None;
            entry.orphaned = false;
            entry.awaited = true;
            if let Some(view) = entry.view() {
                hold::set(
                    entry.session,
                    hold::Hold {
                        ticket,
                        view: view.get(),
                    },
                );
            }
            self.sync_edit_header(ticket);
        }
    }

    /// Withdraws a parked `await edit` waiter (`Wake::EditAwaitWithdraw`).
    pub(crate) fn withdraw_edit_waiter(&mut self, waiter: WaiterId) {
        self.withdraw_edit_waiter_at(waiter, Instant::now());
    }

    /// [`Self::withdraw_edit_waiter`] at `now`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Withdraw",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn withdraw_edit_waiter_at(&mut self, waiter: WaiterId, now: Instant) {
        if self
            .native_documents
            .edits
            .waiters
            .remove(&waiter)
            .is_some()
        {
            let tickets: Vec<Ticket> = self.native_documents.edits.open.keys().copied().collect();
            for ticket in tickets {
                self.start_edit_orphan_clock_if_unwaited(ticket, now);
            }
        }
        self.sweep_orphaned_edits(now);
    }

    fn start_edit_orphan_clock_if_unwaited(&mut self, ticket: Ticket, now: Instant) {
        if self.native_documents.edits.waited(ticket) {
            return;
        }
        let Some(entry) = self.native_documents.edits.open.get_mut(&ticket) else {
            return;
        };
        if entry.view().is_none() || entry.unwaited_since.is_some() {
            return;
        }
        entry.unwaited_since = Some(now);
        hold::set_expiry(entry.session, ticket, Some(now + ORPHAN_AFTER));
    }

    /// Marks open tickets unwaited for [`ORPHAN_AFTER`] by `now` as orphaned.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Orphan",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn sweep_orphaned_edits(&mut self, now: Instant) {
        let mut orphaned = Vec::new();
        for (ticket, entry) in &mut self.native_documents.edits.open {
            // An `edit … wait=0` was never waited on: nothing left it.
            if entry.awaited
                && !entry.orphaned
                && entry
                    .unwaited_since
                    .is_some_and(|since| since + ORPHAN_AFTER <= now)
            {
                entry.orphaned = true;
                hold::clear(entry.session, *ticket);
                orphaned.push(*ticket);
            }
        }
        for ticket in orphaned {
            self.sync_edit_header(ticket);
        }
    }

    /// Sweeps orphaned edit tickets when the event loop parks and arms the next deadline.
    pub(crate) fn sweep_orphaned_edits_at_park(&mut self) {
        if !self.native_documents.edits.open.is_empty() {
            self.sweep_orphaned_edits(Instant::now());
        }
    }

    /// Settles any `edit` ticket bound to `view` when `view` is removed from its window.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Retire",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "PersonCloses",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    /// Run `retire` with the leaves it retires counted as closing in `wid`, the
    /// one window an `edit` ticket may then hand to its caller's tab.
    pub(crate) fn retiring_edit_leaves_in<R>(
        &mut self,
        wid: WindowId,
        retire: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let prior = self.native_documents.edits.retiring_in.replace(wid);
        let retired = retire(self);
        self.native_documents.edits.retiring_in = prior;
        retired
    }

    pub(crate) fn edit_view_retired(&mut self, view: ViewId) {
        let ending = self.native_documents.edits.endings.remove(&view);
        if let Some(ticket) = self.native_documents.edits.ticket_of_view(view) {
            self.resolve_edit(ticket, ending);
        } else if let Some(ending) = ending {
            let outcome = self.ending_outcome(ending, None);
            self.answer_uri_waiters_of(ending.document(), outcome);
        }
        self.answer_view_waiters();
    }

    fn ending_outcome(&self, ending: Ending, start: Option<Seq>) -> EditOutcome {
        let document = ending.document();
        let start = start.unwrap_or(match ending {
            Ending::Finish { from, .. } => from,
            Ending::Abort { .. } => Seq(0),
        });
        self.edit_outcome(document, start, Some(ending))
    }

    fn edit_outcome(
        &self,
        document: DocumentId,
        start: Seq,
        ending: Option<Ending>,
    ) -> EditOutcome {
        let durable = self
            .document_store
            .checkpoint_seq(document)
            .unwrap_or(start);
        let head = self
            .document_store
            .snapshot(document)
            .map_or(durable, |snapshot| snapshot.seq);
        let result = ending.map_or(EditResult::Closed, |ending| ending.result(durable));
        let saved = self.document_store.directory(document).is_none()
            && result != EditResult::Aborted
            && durable > start
            && durable >= head;
        EditOutcome {
            result,
            saved,
            seq: durable.0,
        }
    }

    fn resolve_edit(&mut self, ticket: Ticket, ending: Option<Ending>) {
        let Some(entry) = self.native_documents.edits.open.remove(&ticket) else {
            return;
        };
        let Phase::Open {
            view,
            tab,
            document,
            canonical_uri,
            start,
        } = entry.phase
        else {
            return;
        };
        let behind = self.native_documents.edits.closing_behind == Some(view);
        let outcome = self.edit_outcome(document, start, ending);
        hold::clear(entry.session, ticket);
        self.native_documents.edits.remember(Resolved {
            ticket,
            uri: entry.uri.clone(),
            canonical_uri,
            outcome,
        });
        if ending.is_some() {
            self.answer_uri_waiters_of(document, outcome);
        }
        self.settle_uncovered_edit_waiters();
        // Only the window the edit tab closed in goes to the caller's tab, and only
        // while it holds that tab: a tab moved to another window takes nothing back.
        if !behind
            && let Some(ws) = self
                .native_documents
                .edits
                .retiring_in
                .and_then(|wid| self.windows.get_mut(&wid))
            && ws.tab_set.get(tab).is_none()
            && ws.tab_set.switch_to(entry.caller_tab)
        {
            crate::app_tabs::align_terminal_projection_to_active(ws, &self.view_store);
            ws.last_present = None;
        }
    }

    fn uncovered_edit_waiter(&self, waiter: &Waiter) -> Uncovered {
        let edits = &self.native_documents.edits;
        match &waiter.key {
            AwaitKey::Ticket(ticket) => Uncovered::Answer(
                edits
                    .outcome_of(*ticket)
                    .map_or_else(|| "ERR unknown ticket\n".to_string(), outcome_line),
            ),
            AwaitKey::Uri(_) => {
                if let Some(outcome) = edits.latest_outcome_for(&waiter.spellings) {
                    Uncovered::Answer(outcome_line(outcome))
                } else if self.editor_views_of(&waiter.spellings) == 0 {
                    Uncovered::Answer(outcome_line(self.view_wait_outcome(&waiter.spellings)))
                } else {
                    Uncovered::ByViews
                }
            }
        }
    }

    /// Settles parked waiters whose target tickets or views are all gone.
    pub(crate) fn settle_uncovered_edit_waiters(&mut self) {
        let edits = &self.native_documents.edits;
        let uncovered: Vec<WaiterId> = edits
            .waiters
            .iter()
            .filter(|(_, waiter)| !waiter.by_views && !waiter.covered_by_any(&edits.open))
            .map(|(id, _)| *id)
            .collect();
        for id in uncovered {
            let Some(waiter) = self.native_documents.edits.waiters.get(&id) else {
                continue;
            };
            match self.uncovered_edit_waiter(waiter) {
                Uncovered::Answer(line) => {
                    if let Some(waiter) = self.native_documents.edits.waiters.remove(&id) {
                        let _ = waiter.reply.send(line);
                    }
                }
                Uncovered::ByViews => {
                    if let Some(waiter) = self.native_documents.edits.waiters.get_mut(&id) {
                        waiter.by_views = true;
                    }
                }
            }
        }
    }

    fn answer_view_waiters(&mut self) {
        let done: Vec<(WaiterId, EditOutcome)> = self
            .native_documents
            .edits
            .waiters
            .iter()
            .filter(|(_, waiter)| waiter.by_views && self.editor_views_of(&waiter.spellings) == 0)
            .map(|(id, waiter)| (*id, self.view_wait_outcome(&waiter.spellings)))
            .collect();
        for (id, outcome) in done {
            if let Some(waiter) = self.native_documents.edits.waiters.remove(&id) {
                let _ = waiter.reply.send(outcome_line(outcome));
            }
        }
    }

    fn documents_named(&self, spellings: &[String]) -> Vec<DocumentId> {
        let mut documents: Vec<DocumentId> = Vec::new();
        for document in spellings
            .iter()
            .filter_map(|uri| self.document_store.id_for_uri(uri))
        {
            if !documents.contains(&document) {
                documents.push(document);
            }
        }
        documents
    }

    fn editor_views_of(&self, spellings: &[String]) -> usize {
        self.documents_named(spellings)
            .into_iter()
            .map(|document| {
                self.document_native_views(document)
                    .into_iter()
                    .filter(|(_, instance, _)| {
                        self.native_runtime
                            .app(*instance)
                            .is_some_and(|app| app.kind() == AppKind::Editor)
                    })
                    .count()
            })
            .sum()
    }

    fn view_wait_outcome(&self, spellings: &[String]) -> EditOutcome {
        let seq = self
            .documents_named(spellings)
            .into_iter()
            .find_map(|document| self.document_store.checkpoint_seq(document))
            .map_or(0, |seq| seq.0);
        EditOutcome {
            result: EditResult::Closed,
            saved: false,
            seq,
        }
    }

    /// Whether `view` belongs to an open `edit` ticket whose tab still carries the attention mark.
    pub(crate) fn edit_view_attention(&self, view: ViewId) -> bool {
        self.native_documents
            .edits
            .open
            .values()
            .any(|entry| entry.attention && entry.view() == Some(view))
    }

    /// Whether the tab in `wid` containing `view` carries an `edit` attention mark.
    pub(crate) fn edit_attention_in_tab_of(&self, wid: WindowId, view: ViewId) -> bool {
        let Some(tab) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.tabs().iter().find(|tab| tab.root.contains(view)))
        else {
            return false;
        };
        tab.root
            .any_leaf(&mut |&leaf| self.edit_view_attention(leaf))
    }

    /// Clears `edit` attention marks on all views in `wid`'s active tab.
    pub(crate) fn edit_tabs_seen(&mut self, wid: WindowId) {
        let Some(active) = self.windows.get(&wid).and_then(|ws| ws.tab_set.active()) else {
            return;
        };
        for entry in self.native_documents.edits.open.values_mut() {
            if entry.attention && entry.view().is_some_and(|view| active.root.contains(view)) {
                entry.attention = false;
            }
        }
    }

    /// Enables serving `edit` requests on a headless test `App`.
    #[cfg(test)]
    pub(crate) fn serve_edits_headless_for_test(&mut self) {
        self.native_documents.edits.serve_headless = true;
    }

    /// Whether `ticket` was requested with `create=1` (allowing a missing file).
    pub(crate) fn edit_admits_missing(&self, ticket: EditTicketId) -> bool {
        self.native_documents
            .edits
            .open
            .get(&ticket)
            .is_some_and(|entry| entry.create)
    }

    /// Synchronizes the Editor header bar on `ticket`'s view with its current waiter state.
    pub(crate) fn sync_edit_header(&mut self, ticket: EditTicketId) {
        let Some((view, window, header)) = self
            .native_documents
            .edits
            .open
            .get(&ticket)
            .and_then(|entry| Some((entry.view()?, entry.window, entry.header())))
        else {
            return;
        };
        let Some(crate::native_app::AppViewState::Editor(state)) =
            self.native_runtime.view_state_mut(view)
        else {
            return;
        };
        if state.edit_header.as_ref() == Some(&header) {
            return;
        }
        state.edit_header = Some(header);
        state.common.presentation_revision = state.common.presentation_revision.saturating_add(1);
        self.invalidate_native_view_cache(window, view, crate::native_app::DamageRegion::All);
        self.request_native_redraw(window);
    }

    fn editor_view_document(&self, view: ViewId) -> Option<DocumentId> {
        let crate::tab_model::View::Native(native) = self.view_store.get(view).copied()? else {
            return None;
        };
        self.native_runtime
            .app(native.instance)
            .is_some_and(|app| app.kind() == AppKind::Editor)
            .then(|| self.native_runtime.document_id(native.instance))
            .flatten()
    }

    fn uri_waiters_of(&self, document: DocumentId) -> Vec<WaiterId> {
        self.native_documents
            .edits
            .waiters
            .iter()
            .filter(|(_, waiter)| {
                matches!(waiter.key, AwaitKey::Uri(_))
                    && self.documents_named(&waiter.spellings).contains(&document)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    fn answer_uri_waiters_of(&mut self, document: DocumentId, outcome: EditOutcome) {
        for id in self.uri_waiters_of(document) {
            if let Some(waiter) = self.native_documents.edits.waiters.remove(&id) {
                let _ = waiter.reply.send(outcome_line(outcome));
            }
        }
    }

    /// Handles `finish-edit` in `view`, saving the document and closing the view once saved.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Finish",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "FinishHalts",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn finish_edit_view(&mut self, view: ViewId) {
        self.sweep_orphaned_edits(Instant::now());
        let Some(document) = self.editor_view_document(view) else {
            return;
        };
        if self.native_documents.edits.ticket_of_view(view).is_none()
            && self.uri_waiters_of(document).is_empty()
        {
            self.set_editor_view_status(view, NO_EDIT_WAITING);
            return;
        }
        let (Some(snapshot), Some(from)) = (
            self.document_store.snapshot(document),
            self.document_store.checkpoint_seq(document),
        ) else {
            return;
        };
        let seq = snapshot.seq;
        self.native_documents.edits.endings.insert(
            view,
            Ending::Finish {
                document,
                seq,
                from,
            },
        );
        if from >= seq {
            self.close_finished_edit_view(view);
            return;
        }
        self.set_editor_view_status(view, "Saving, then closing");
        if let Err(why) = self.save_document_checkpoint(document, view) {
            self.native_documents.edits.endings.remove(&view);
            self.set_editor_view_status(view, &format!("Not done: {why}"));
        }
    }

    fn close_finished_edit_view(&mut self, view: ViewId) {
        if let Err(why) = self.close_edit_view_in_place(view)
            && self.view_store.get(view).is_some()
        {
            let status = self.editor_close_status(view, None, &why);
            self.set_editor_view_status(view, &status);
        }
    }

    fn close_edit_view_in_place(&mut self, view: ViewId) -> Result<(), String> {
        let place = self.windows.iter().find_map(|(wid, ws)| {
            let tab = ws
                .tab_set
                .tabs()
                .iter()
                .find(|tab| tab.root.contains(view))?;
            let front = ws.tab_set.active().map(|front| (front.id, front.focus));
            Some((*wid, tab.id, tab.root.len() == 1, front))
        });
        let Some((wid, tab, whole, Some((front, focus)))) = place else {
            return self.close_editor_view(view);
        };
        if front == tab && focus == view {
            return self.close_editor_view(view);
        }
        self.native_documents.edits.closing_behind = Some(view);
        let closed = if whole {
            // Selected for its close preflight only; the tab in front is put
            // back in the same topology edit, before any sync.
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.tab_set.switch_to(tab);
            }
            self.close_active_native_tab_keeping(wid, Some(front))
        } else {
            self.close_editor_view(view)
        };
        self.native_documents.edits.closing_behind = None;
        if let Some(ws) = self.windows.get_mut(&wid) {
            let mut moved = ws.tab_set.active_id() != Some(front) && ws.tab_set.switch_to(front);
            if let Some(active) = ws.tab_set.active_mut()
                && active.id == front
                && active.focus != focus
                && active.set_focus(focus)
            {
                moved = true;
            }
            if moved {
                crate::app_tabs::align_terminal_projection_to_active(ws, &self.view_store);
                ws.last_present = None;
                self.resync_active_or_window(wid);
                self.request_native_redraw(wid);
            }
        }
        closed
    }

    /// Completes any pending `finish-edit` on `document` after a save finishes.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "FinishHalts",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn edit_document_saved(&mut self, document: DocumentId, failure: Option<&str>) {
        let finishing: Vec<(ViewId, Seq)> = self
            .native_documents
            .edits
            .endings
            .iter()
            .filter_map(|(view, ending)| match *ending {
                Ending::Finish {
                    document: of, seq, ..
                } if of == document => Some((*view, seq)),
                _ => None,
            })
            .collect();
        if finishing.is_empty() {
            return;
        }
        let durable = self.document_store.checkpoint_seq(document);
        for (view, seq) in finishing {
            if durable.is_some_and(|durable| durable >= seq) {
                self.close_finished_edit_view(view);
            } else if !self.document_save_owed(document) {
                self.native_documents.edits.endings.remove(&view);
                let why = failure.unwrap_or("the save did not reach the text you finished");
                self.set_editor_view_status(view, &format!("Not done: {why}"));
            }
        }
    }

    /// Tracks foreign edits on `document` when `source_view` commits sequence `seq`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "PersonEdits",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn edit_document_committed(
        &mut self,
        document: DocumentId,
        source_view: ViewId,
        seq: Seq,
    ) {
        for entry in self.native_documents.edits.open.values_mut() {
            if let Phase::Open {
                view, document: of, ..
            } = entry.phase
                && of == document
                && view != source_view
            {
                entry.foreign_at = Some(seq);
            }
        }
    }

    fn other_editor_views(&self, document: DocumentId, view: ViewId) -> Vec<ViewId> {
        self.document_native_views(document)
            .into_iter()
            .filter(|(_, instance, other)| {
                *other != view
                    && self
                        .native_runtime
                        .app(*instance)
                        .is_some_and(|app| app.kind() == AppKind::Editor)
            })
            .map(|(_, _, other)| other)
            .collect()
    }

    /// Handles `abort-edit` in `view`, discarding unsaved changes and closing the view.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Abort",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "AbortHalts",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn abort_edit_view(&mut self, view: ViewId) {
        self.sweep_orphaned_edits(Instant::now());
        let Some(document) = self.editor_view_document(view) else {
            return;
        };
        let others = self.other_editor_views(document, view);
        let durable = self.document_store.checkpoint_seq(document);
        let ticket = self
            .native_documents
            .edits
            .ticket_of_view(view)
            .and_then(|ticket| self.native_documents.edits.open.get(&ticket));
        let foreign = ticket.map_or(!others.is_empty(), |entry| {
            entry.holds_foreign_text(durable)
        });
        let refusal = match ticket {
            Some(entry) if entry.client_waits() => None,
            _ if !self.uri_waiters_of(document).is_empty() => None,
            Some(entry) if entry.awaited => Some(ABORT_GONE),
            Some(_) => Some(ABORT_UNWAITED),
            None => Some(NO_EDIT_WAITING),
        }
        .or_else(|| self.document_save_owed(document).then_some(ABORT_SAVING))
        .or_else(|| (foreign && others.is_empty()).then_some(ABORT_FOREIGN));
        if let Some(refusal) = refusal {
            self.set_editor_view_status(view, refusal);
            return;
        }
        let pressed = self
            .native_documents
            .edits
            .endings
            .insert(view, Ending::Abort { document });
        if !foreign && let Err(why) = self.discard_document_changes(document, view) {
            match pressed {
                Some(pressed) => self.native_documents.edits.endings.insert(view, pressed),
                None => self.native_documents.edits.endings.remove(&view),
            };
            self.set_editor_view_status(view, &format!("Not aborted: {why}"));
            return;
        }
        if self.view_store.get(view).is_none() {
            return;
        }
        if let Err(why) = self.close_edit_view_in_place(view)
            && self.view_store.get(view).is_some()
        {
            self.native_documents.edits.endings.remove(&view);
            let status = if foreign {
                format!("Nothing was discarded; this buffer stays open: {why}")
            } else {
                format!("Changes discarded; this buffer stays open: {why}")
            };
            self.set_editor_view_status(view, &status);
            return;
        }
        if foreign {
            for other in others {
                self.set_editor_view_status(other, ABORT_KEPT);
            }
        }
    }
}

/// The model's two actions only a mutant enables have no seam to bind. A
/// module-level carrier under the anchors' own cfg, not one in the cfg(test)
/// conformance file, so aterm-models' closure links these waivers too.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AmacsEditTicket",
    action = "AnswerAtRequest",
    reason = "Buggy = 1 only: an answer before checkpoint_ack, which no shipping seam performs; \
              answer_before_ack_is_caught drives the rigged run and requires it be rejected."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AmacsEditTicket",
    action = "ResolveAgain",
    reason = "Buggy = 2 only: a second outcome for a resolved ticket; resolve_edit removes the \
              ticket from the open map before it records the outcome, so none exists to bind."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn mutant_only_actions() {}

#[cfg(test)]
#[path = "edit_ticket_conformance.rs"]
mod edit_ticket_conformance;

#[cfg(test)]
#[path = "markdown_workspace_tests.rs"]
mod markdown_workspace_tests;

#[cfg(test)]
#[path = "app_edit_tests.rs"]
mod tests;
