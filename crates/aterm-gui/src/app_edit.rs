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
        if self.headless && !self.native_documents.edits.serves_headless() {
            return refuse("ERR unsupported headless\n");
        }
        let Some(window) = self.window_of_session(session) else {
            return refuse("ERR no-window\n");
        };
        let Some(caller_tab) = self.tab_hosting_session(window, session) else {
            return refuse("ERR no-window\n");
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
                    .leaves()
                    .into_iter()
                    .any(|view| {
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
        hold::set(
            session,
            hold::Hold {
                ticket,
                view: installed.view.get(),
            },
        );
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

    /// Places `ticket`'s newly created `tab` immediately to the right of its caller tab.
    pub(crate) fn place_admitted_view(
        &mut self,
        wid: WindowId,
        tab: TabId,
        ticket: EditTicketId,
        previously_active: Option<TabId>,
    ) {
        let Some(caller_tab) = self
            .native_documents
            .edits
            .open
            .get(&ticket)
            .map(|entry| entry.caller_tab)
        else {
            return;
        };
        let front = self.frontmost_window == Some(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(caller) = ws.tab_set.tabs().iter().position(|t| t.id == caller_tab) {
            ws.tab_set.reorder(tab, caller + 1);
        }
        let keeps_focus = front && previously_active == Some(caller_tab);
        if !keeps_focus
            && let Some(previous) = previously_active
            && ws.tab_set.switch_to(previous)
        {
            crate::app_tabs::align_terminal_projection_to_active(ws, &self.view_store);
        }
        let behind = ws.tab_set.active_id() != Some(tab);
        ws.last_present = None;
        if let Some(entry) = self.native_documents.edits.open.get_mut(&ticket) {
            entry.attention = behind;
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
        let saved = result != EditResult::Aborted && durable > start && durable >= head;
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
            .leaves()
            .into_iter()
            .any(|leaf| self.edit_view_attention(leaf))
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
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{Receiver, TryRecvError};

    use amacs::protocol::edit::EditRequest;

    use super::*;
    use crate::app_documents::{DocumentAdmissionOutcome, DocumentAdmissionRejected};
    use crate::native_app::{AppEvent, AppViewState, TextInputEvent};

    /// Scratch file fixture for `app_edit` tests.
    pub(super) struct Fixture {
        _dir: aterm_tempfile::TempDir,
        pub(super) path: PathBuf,
        pub(super) uri: String,
    }

    pub(super) fn fixture(name: &str, bytes: &[u8]) -> Fixture {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-edit-{name}-"))
            .tempdir()
            .unwrap();
        let path = dir.path().join("COMMIT_EDITMSG");
        std::fs::write(&path, bytes).unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        Fixture {
            _dir: dir,
            path,
            uri,
        }
    }

    /// Creates a headless test `App` with `edit` enabled.
    pub(super) fn app() -> App {
        let mut app = App::headless_for_test();
        app.serve_edits_headless_for_test();
        app
    }

    /// Creates a front terminal tab in `wid` with a unique session ID.
    pub(super) fn caller(app: &mut App, wid: WindowId) -> u64 {
        let sid = fresh_session(app);
        app.push_stub_tab(wid, crate::stub_session(sid));
        app.next_session_id = sid + 1;
        sid
    }

    /// Reserves a unique session ID block on `app`.
    pub(super) fn fresh_session(app: &mut App) -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1 << 40);
        let sid = NEXT.fetch_add(1 << 12, Ordering::Relaxed);
        app.next_session_id = sid;
        sid
    }

    pub(super) fn request(uri: &str, line: Option<(u32, Option<u32>)>) -> EditRequest {
        EditRequest {
            uri: uri.to_string(),
            line,
            create: false,
            for_word: None,
            wait: true,
        }
    }

    /// Executes `@<session> edit` inline and returns its reply line.
    pub(super) fn edit(app: &mut App, session: u64, request: EditRequest) -> (Ticket, String) {
        let ticket = crate::control_edit::mint_ticket();
        let (reply, answer) = std::sync::mpsc::channel();
        app.begin_edit(call(ticket, session, request, reply));
        (
            ticket,
            answer.try_recv().expect("an inline edit answers at once"),
        )
    }

    /// Builds an [`EditCall`] with resolved realpath URI and reply receiver.
    pub(super) fn call(
        ticket: Ticket,
        session: u64,
        request: EditRequest,
        reply: std::sync::mpsc::Sender<String>,
    ) -> EditCall {
        EditCall {
            ticket,
            session,
            real_uri: crate::control_edit::real_file_uri(&request.uri),
            request,
            reply,
        }
    }

    /// Parks an `await edit` request for `key` on `app`.
    pub(super) fn wait(app: &mut App, key: AwaitKey) -> (WaiterId, Receiver<String>) {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let waiter = WaiterId(NEXT.fetch_add(1, Ordering::Relaxed));
        let (reply, answer) = std::sync::mpsc::channel();
        let real_uri = match &key {
            AwaitKey::Uri(uri) => crate::control_edit::real_file_uri(uri),
            AwaitKey::Ticket(_) => None,
        };
        app.await_edit(AwaitCall {
            waiter,
            key,
            real_uri,
            reply,
        });
        (waiter, answer)
    }

    pub(super) fn tab_ids(app: &App, wid: WindowId) -> Vec<TabId> {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect()
    }

    pub(super) fn active_tab(app: &App, wid: WindowId) -> TabId {
        app.windows[&wid]
            .tab_set
            .active_id()
            .expect("an active tab")
    }

    /// Returns the [`TabId`] in `wid` that contains `view`.
    pub(super) fn tab_of(app: &App, wid: WindowId, view: ViewId) -> TabId {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .find(|tab| tab.root.contains(view))
            .map(|tab| tab.id)
            .expect("the view's tab")
    }

    pub(super) fn select(app: &mut App, wid: WindowId, tab: TabId) {
        assert!(app.windows.get_mut(&wid).unwrap().tab_set.switch_to(tab));
        app.sync_window(wid);
    }

    /// Focuses split leaf `view` in `wid`'s active tab.
    pub(super) fn focus_leaf(app: &mut App, wid: WindowId, view: ViewId) {
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(view);
        app.sync_window(wid);
    }

    fn attention(app: &App, wid: WindowId, tab: TabId) -> bool {
        app.windows[&wid]
            .tab_set
            .get(tab)
            .is_some_and(|tab| tab.presentation.indicators.attention)
    }

    pub(super) fn type_text(app: &mut App, wid: WindowId, text: &str) {
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
        )
        .unwrap();
    }

    fn ok(line: &str) -> EditOk {
        EditOk::parse(line).unwrap_or_else(|error| panic!("{line:?}: {error}"))
    }

    fn view(ok: &EditOk) -> ViewId {
        ViewId::from_stored(ok.view)
    }

    fn document_of(app: &App, view: ViewId) -> DocumentId {
        let Some(crate::tab_model::View::Native(native)) = app.view_store.get(view).copied() else {
            panic!("view {view:?} is not a native view");
        };
        app.native_runtime
            .document_id(native.instance)
            .expect("a document view")
    }

    fn orphaned(app: &App, ticket: Ticket) -> bool {
        app.native_documents.edits.open[&ticket].orphaned
    }

    #[test]
    fn edit_opens_fresh_view_beside_caller_even_when_document_already_open() {
        let text = "one\ntwo\nthree\nfour\n";
        let f = fixture("fresh", text.as_bytes());
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        let neighbour = caller(&mut app, wid);
        let _ = neighbour;
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, person_view) = app.active_native_view(wid).unwrap();
        let person_tab = active_tab(&app, wid);
        select(&mut app, wid, caller_tab);

        let before = Instant::now();
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, Some((3, None))));
        let ok = ok(&line);
        assert_eq!(ok.ticket, ticket);
        let canonical =
            crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&f.path).unwrap())
                .unwrap();
        assert_eq!(ok.uri, canonical);
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        let edit_tab = tabs[at + 1];
        assert_ne!(edit_tab, person_tab, "a new tab, right of the caller's");
        assert_eq!(active_tab(&app, wid), edit_tab, "it takes focus");
        let (_, front_view) = app.active_native_view(wid).unwrap();
        assert_eq!(front_view, view(&ok));
        assert_ne!(view(&ok), person_view, "the person's view is never reused");
        assert_eq!(document_of(&app, view(&ok)), document_of(&app, person_view));
        assert_eq!(ok.doc, document_of(&app, person_view).get());
        assert!(!attention(&app, wid, person_tab), "nor marked");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view(&ok)) else {
            panic!("an Editor view");
        };
        let caret = crate::path_link::line_col_offset(text, 3, None);
        assert_eq!(
            state.buffer.as_ref().unwrap().selections,
            vec![crate::native_editor::Selection::caret(caret)]
        );
        assert_eq!(
            hold::get_at(sid, before),
            Some(hold::Hold {
                ticket,
                view: ok.view
            })
        );
    }

    #[test]
    fn edit_admission_never_coalesces_with_a_gesture_open() {
        let f = fixture("coalesce", b"draft\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let gesture = DocumentAdmissionKey {
            uri: f.uri.clone(),
            kind: AppKind::Editor,
            edit: None,
        };
        let pending = app.mint_document_admission(wid, gesture.clone());

        let (first, line) = edit(&mut app, sid, request(&f.uri, None));
        let first_ok = ok(&line);
        assert_eq!(
            app.document_admission_pending(wid, &gesture),
            Some(pending),
            "the edit read on its own; the gesture's read is untouched"
        );
        let (second, line) = edit(&mut app, sid, request(&f.uri, None));
        let second_ok = ok(&line);
        assert_ne!(first, second);
        assert_ne!(
            first_ok.view, second_ok.view,
            "each ticket owns its own view"
        );
        assert_eq!(first_ok.doc, second_ok.doc);

        let posted = |request| {
            let (reply, _answer) = std::sync::mpsc::channel();
            call(crate::control_edit::mint_ticket(), sid, request, reply)
        };
        let (_, a, a_ticket) = app
            .begin_edit_in_flight_for_test(posted(request(&f.uri, None)))
            .unwrap();
        let (_, b, b_ticket) = app
            .begin_edit_in_flight_for_test(posted(request(&f.uri, None)))
            .unwrap();
        assert_ne!(a, b);
        assert_ne!(a, gesture);
        assert_ne!(a_ticket, b_ticket);
        assert_eq!(app.document_admission_pending(wid, &a), Some(a_ticket));
        assert_eq!(app.document_admission_pending(wid, &b), Some(b_ticket));
    }

    #[test]
    fn edit_uses_the_callers_window_not_the_frontmost() {
        let f = fixture("window", b"x\n");
        let mut app = app();
        let front = WindowId(0);
        let sid = fresh_session(&mut app);
        let other = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        app.frontmost_window = Some(front);
        let front_tabs = tab_ids(&app, front);
        let caller_tab = active_tab(&app, other);

        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let ok = ok(&line);
        assert_eq!(
            tab_ids(&app, front),
            front_tabs,
            "nothing lands in the front window"
        );
        let tabs = tab_ids(&app, other);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0], caller_tab);
        assert!(
            app.windows[&other]
                .tab_set
                .get(tabs[1])
                .unwrap()
                .root
                .contains(view(&ok)),
            "the view is in the caller's window"
        );
        assert_eq!(active_tab(&app, other), caller_tab);
        assert!(attention(&app, other, tabs[1]));
    }

    #[test]
    fn focus_follows_the_askpass_rule() {
        let f = fixture("focus", b"x\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);

        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let first = ok(&line);
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        assert_eq!(active_tab(&app, wid), tabs[at + 1]);
        assert!(!attention(&app, wid, tabs[at + 1]));
        assert!(
            app.windows[&wid]
                .tab_set
                .get(tabs[at + 1])
                .unwrap()
                .root
                .contains(view(&first))
        );

        let _other = caller(&mut app, wid);
        let in_front = active_tab(&app, wid);
        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let second = ok(&line);
        assert_eq!(active_tab(&app, wid), in_front);
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        let placed = tabs[at + 1];
        assert!(
            app.windows[&wid]
                .tab_set
                .get(placed)
                .unwrap()
                .root
                .contains(view(&second))
        );
        assert!(attention(&app, wid, placed));
    }

    #[test]
    fn dirty_close_answers_closed_only_after_checkpoint_ack() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);

        let f = fixture("dirty-leaf", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let ok = ok(&line);
        let edit_tab = active_tab(&app, wid);
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        let (_, terminal) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        focus_leaf(&mut app, wid, view(&ok));
        type_text(&mut app, wid, "edited ");
        let document = document_of(&app, view(&ok));
        let held = app.hold_document_save_for_test(document);
        assert!(
            app.close_focused_mixed_leaf(wid).is_err(),
            "the close waits"
        );
        assert!(app.document_close_pending_for_test(document));
        assert_eq!(
            answer.try_recv(),
            Err(TryRecvError::Empty),
            "no answer before the ack"
        );
        assert!(app.view_store.get(view(&ok)).is_some());

        app.land_held_document_save_for_test(document, view(&ok), held)
            .unwrap();
        let line = answer
            .try_recv()
            .expect("answered once the save is durable");
        let outcome = EditOutcome::parse(&line).unwrap();
        let durable = app.document_store.checkpoint_seq(document).unwrap();
        assert_eq!(
            outcome,
            EditOutcome {
                result: EditResult::Closed,
                saved: true,
                seq: durable.0
            }
        );
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "edited base\n");
        assert!(app.view_store.get(view(&ok)).is_none());
        assert!(
            app.view_store.get(terminal).is_some(),
            "the rest of the tab stays"
        );
        assert_eq!(active_tab(&app, wid), edit_tab, "its tab is still there");
        assert_ne!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
        let (_, late) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(late.try_recv().unwrap(), line);

        select(&mut app, wid, caller_tab);
        let g = fixture("dirty-tab", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let tab_view = view(&self::ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "again ");
        let document = document_of(&app, tab_view);
        let held = app.hold_document_save_for_test(document);
        assert!(
            app.close_active_native_tab(wid).is_err(),
            "refused while saving"
        );
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
        app.land_held_document_save_for_test(document, tab_view, held)
            .unwrap();
        assert_eq!(
            answer.try_recv(),
            Err(TryRecvError::Empty),
            "the view is still open"
        );
        assert!(app.view_store.get(tab_view).is_some());
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
        assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, true));
        assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "again base\n");
        assert_eq!(
            active_tab(&app, wid),
            caller_tab,
            "handed back to the caller"
        );
    }

    #[test]
    fn hangup_withdraws_and_orphans_after_5s_and_keeps_the_view() {
        let f = fixture("orphan", b"keep\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let ok = ok(&line);
        let (waiter, _answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        let held = Some(hold::Hold {
            ticket,
            view: ok.view,
        });
        assert_eq!(hold::get(sid), held);

        let left = Instant::now();
        app.withdraw_edit_waiter_at(waiter, left);
        app.sweep_orphaned_edits(left);
        assert!(!orphaned(&app, ticket));
        assert_eq!(hold::get_at(sid, left), held);
        let later = left + ORPHAN_AFTER;
        app.sweep_orphaned_edits(later);
        assert!(orphaned(&app, ticket));
        assert_eq!(hold::get(sid), None);
        assert!(
            app.view_store.get(view(&ok)).is_some(),
            "an orphan keeps its view"
        );
        assert_eq!(app.active_native_view(wid).map(|(_, v)| v), Some(view(&ok)));

        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert!(!orphaned(&app, ticket));
        assert_eq!(hold::get(sid), held);
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
        assert_eq!(outcome.result, EditResult::Closed);
        assert!(!outcome.saved, "nothing was saved");
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "keep\n");
    }

    #[test]
    fn await_by_uri_ignores_views_the_person_opened() {
        let f = fixture("uri", b"shared\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let person_tab = active_tab(&app, wid);
        select(&mut app, wid, caller_tab);
        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_tab = active_tab(&app, wid);
        let ok = ok(&line);
        let (_, by_request) = wait(&mut app, AwaitKey::Uri(f.uri.clone()));
        let (_, by_canonical) = wait(&mut app, AwaitKey::Uri(ok.uri.clone()));

        select(&mut app, wid, person_tab);
        app.close_active_native_tab(wid).unwrap();
        assert_eq!(by_request.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(by_canonical.try_recv(), Err(TryRecvError::Empty));

        select(&mut app, wid, edit_tab);
        app.close_active_native_tab(wid).unwrap();
        for answer in [by_request, by_canonical] {
            let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
            assert_eq!(outcome.result, EditResult::Closed);
        }

        let g = fixture("uri-successor", b"theirs\n");
        app.open_document_tab(AppKind::Editor, &g.uri).unwrap();
        let canonical =
            crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&g.path).unwrap())
                .unwrap();
        let (_, successor) = wait(&mut app, AwaitKey::Uri(canonical));
        assert_eq!(successor.try_recv(), Err(TryRecvError::Empty));
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&successor.try_recv().unwrap()).unwrap();
        assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, false));
    }

    #[test]
    fn edit_on_headless_answers_unsupported() {
        let f = fixture("headless", b"x\n");
        let mut app = App::headless_for_test();
        let (_, line) = edit(&mut app, 0, request(&f.uri, None));
        assert_eq!(line, "ERR unsupported headless\n");
        assert!(app.native_documents.edits.open.is_empty());

        let mut app = self::app();
        let sid = caller(&mut app, WindowId(0));
        assert_eq!(
            edit(&mut app, sid + 1_000, request(&f.uri, None)).1,
            "ERR no-window\n"
        );
        assert!(app.native_documents.edits.open.is_empty());
    }

    #[test]
    fn edit_refusals_name_the_reason() {
        let f = fixture("refusals", b"\xff\xfe not utf-8\n");
        let mut app = app();
        let sid = caller(&mut app, WindowId(0));
        let tabs = tab_ids(&app, WindowId(0));
        let missing =
            crate::native_document_host::path_to_file_uri(&f.path.with_extension("gone")).unwrap();
        let directory =
            crate::native_document_host::path_to_file_uri(f.path.parent().unwrap()).unwrap();
        for (uri, refusal) in [
            (f.uri.as_str(), "ERR refused invalid-utf8\n"),
            (missing.as_str(), "ERR refused missing\n"),
            (directory.as_str(), "ERR refused not-regular\n"),
        ] {
            assert_eq!(edit(&mut app, sid, request(uri, None)).1, refusal, "{uri}");
        }
        assert_eq!(tab_ids(&app, WindowId(0)), tabs, "nothing opened");
        assert!(app.native_documents.edits.open.is_empty());
    }

    #[test]
    fn timeout_cancels_and_a_late_admission_is_dropped() {
        let f = fixture("late", b"late\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let tabs = tab_ids(&app, wid);
        let ticket = crate::control_edit::mint_ticket();
        let (reply, answer) = std::sync::mpsc::channel();
        let (window, key, admission) = app
            .begin_edit_in_flight_for_test(call(ticket, sid, request(&f.uri, None), reply))
            .unwrap();
        app.cancel_edit(ticket);
        assert_eq!(answer.try_recv(), Err(TryRecvError::Disconnected));

        let result = crate::native_document_host::admit_local_file(
            &key.uri,
            crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
            false,
        )
        .map(Box::new)
        .map_err(DocumentOpenFailure);
        let rejected = app
            .complete_document_admission(DocumentAdmissionOutcome {
                ticket: admission,
                wid: window,
                key,
                result,
            })
            .unwrap_err();
        assert!(
            matches!(rejected, DocumentAdmissionRejected::Dropped(_)),
            "{rejected:?}"
        );
        assert_eq!(tab_ids(&app, wid), tabs, "a late admission opens nothing");
        let (_, unknown) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(unknown.try_recv().unwrap(), "ERR unknown ticket\n");
    }

    #[test]
    fn a_background_edit_keeps_its_mark_until_the_person_sees_it() {
        let text = "one\ntwo\nthree\n";
        let f = fixture("mark", text.as_bytes());
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, person_view) = app.active_native_view(wid).unwrap();
        let person_tab = active_tab(&app, wid);

        let (_, line) = edit(&mut app, sid, request(&f.uri, Some((2, None))));
        let ok = ok(&line);
        let placed = tab_of(&app, wid, view(&ok));
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        assert_eq!(tabs[at + 1], placed, "right of the caller's tab");
        assert_eq!(active_tab(&app, wid), person_tab, "the person's tab stays");
        assert!(attention(&app, wid, placed), "opened behind, marked");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view(&ok)) else {
            panic!("an Editor view");
        };
        assert_eq!(
            state.buffer.as_ref().unwrap().selections,
            vec![crate::native_editor::Selection::caret(
                crate::path_link::line_col_offset(text, 2, None)
            )],
            "the caret is on line 2 behind the person's tab"
        );

        app.refresh_disambiguated_document_titles();
        app.refresh_aggregate_tab_presentation(wid, placed);
        assert!(attention(&app, wid, placed), "a refresh keeps the mark");
        type_text(&mut app, wid, "saved ");
        let document = document_of(&app, person_view);
        let held = app.hold_document_save_for_test(document);
        app.land_held_document_save_for_test(document, person_view, held)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&f.path).unwrap(),
            "saved one\ntwo\nthree\n"
        );
        assert!(attention(&app, wid, placed), "a save elsewhere keeps it");

        select(&mut app, wid, placed);
        assert!(!attention(&app, wid, placed));
        select(&mut app, wid, person_tab);
        app.refresh_aggregate_tab_presentation(wid, placed);
        assert!(!attention(&app, wid, placed), "seen once, spent");
    }

    #[test]
    fn a_cancel_after_the_view_opened_orphans_it_unless_a_waiter_holds_it() {
        let f = fixture("cancel-open", b"x\n");
        let g = fixture("cancel-held", b"y\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let first = view(&ok(&line));
        app.cancel_edit(ticket);
        assert!(orphaned(&app, ticket));
        assert_ne!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
        assert!(
            app.view_store.get(first).is_some(),
            "an orphan keeps its view"
        );

        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let second = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Uri(g.uri.clone()));
        app.cancel_edit(ticket);
        assert!(!orphaned(&app, ticket), "the waiter holds it");
        assert_eq!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
        let tab = tab_of(&app, wid, second);
        select(&mut app, wid, tab);
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
        assert_eq!(outcome.result, EditResult::Closed);
    }

    #[test]
    fn a_refused_or_cancelled_admission_settles_the_waiters_it_covered() {
        let f = fixture("settle", b"x\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);

        let ticket = crate::control_edit::mint_ticket();
        let (reply, _cancelled) = std::sync::mpsc::channel();
        app.begin_edit_in_flight_for_test(call(ticket, sid, request(&f.uri, None), reply))
            .unwrap();
        let (_, by_uri) = wait(&mut app, AwaitKey::Uri(f.uri.clone()));
        let (_, by_ticket) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(by_uri.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(by_ticket.try_recv(), Err(TryRecvError::Empty));
        app.cancel_edit(ticket);
        let outcome =
            EditOutcome::parse(&by_uri.try_recv().expect("settled at the cancel")).unwrap();
        assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, false));
        assert_eq!(by_ticket.try_recv().unwrap(), "ERR unknown ticket\n");

        let missing =
            crate::native_document_host::path_to_file_uri(&f.path.with_extension("gone")).unwrap();
        let ticket = crate::control_edit::mint_ticket();
        let (reply, refused) = std::sync::mpsc::channel();
        let (window, key, admission) = app
            .begin_edit_in_flight_for_test(call(ticket, sid, request(&missing, None), reply))
            .unwrap();
        let (_, by_uri) = wait(&mut app, AwaitKey::Uri(missing));
        let result = crate::native_document_host::admit_local_file(
            &key.uri,
            crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
            false,
        )
        .map(Box::new)
        .map_err(DocumentOpenFailure);
        let _ = app.complete_document_admission(DocumentAdmissionOutcome {
            ticket: admission,
            wid: window,
            key,
            result,
        });
        assert_eq!(refused.try_recv().unwrap(), "ERR refused missing\n");
        let outcome =
            EditOutcome::parse(&by_uri.try_recv().expect("settled at the refusal")).unwrap();
        assert_eq!(
            outcome,
            EditOutcome {
                result: EditResult::Closed,
                saved: false,
                seq: 0
            }
        );
        assert!(app.native_documents.edits.waiters.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_uri_wait_by_another_spelling_of_the_file_finds_it() {
        let alias = |fixture: &Fixture| {
            let dir = fixture.path.parent().unwrap();
            std::os::unix::fs::symlink(dir, dir.join("alias")).unwrap();
            crate::native_document_host::path_to_file_uri(&dir.join("alias/COMMIT_EDITMSG"))
                .unwrap()
        };
        let f = fixture("spelling-ticket", b"x\n");
        let g = fixture("spelling-views", b"y\n");
        let (f_alias, g_alias) = (alias(&f), alias(&g));
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);

        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_view = view(&ok(&line));
        let (_, by_alias) = wait(&mut app, AwaitKey::Uri(f_alias));
        assert_eq!(by_alias.try_recv(), Err(TryRecvError::Empty));
        let tab = tab_of(&app, wid, edit_view);
        select(&mut app, wid, tab);
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&by_alias.try_recv().unwrap()).unwrap();
        assert_eq!(outcome.result, EditResult::Closed);

        app.open_document_tab(AppKind::Editor, &g.uri).unwrap();
        let (_, successor) = wait(&mut app, AwaitKey::Uri(g_alias));
        assert_eq!(
            successor.try_recv(),
            Err(TryRecvError::Empty),
            "the view is still open"
        );
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&successor.try_recv().unwrap()).unwrap();
        assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, false));
    }

    #[test]
    fn edit_and_await_edit_answer_on_the_wire() {
        use crate::control::control_media::{Hops, MainPost, take_hop};
        fn slice(left: Duration) -> Duration {
            left.min(Duration::from_millis(5))
        }
        const HANG: Duration = Duration::from_secs(60);
        let text = "one\ntwo\nthree\nfour\n";
        let f = fixture("wire", text.as_bytes());
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);

        let hops: &'static Hops = Box::leak(Box::new(Hops::new()));
        let line = crate::control_edit::serve_edit(
            crate::control::Scope::Owner,
            Some(sid),
            &format!("{} line=3", f.uri),
            &mut MainPost::injected(
                |wake| match take_hop(wake) {
                    crate::Wake::EditRequest(call) => {
                        app.begin_edit(*call);
                        true
                    }
                    other => panic!("unexpected {other:?}"),
                },
                hops,
            ),
            &mut || false,
            HANG,
            slice,
        );
        assert!(line.starts_with("OK edit ticket="), "{line}");
        let ok = ok(&line);
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        assert_eq!(tab_of(&app, wid, view(&ok)), tabs[at + 1]);
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view(&ok)) else {
            panic!("an Editor view");
        };
        assert_eq!(
            state.buffer.as_ref().unwrap().selections,
            vec![crate::native_editor::Selection::caret(
                crate::path_link::line_col_offset(text, 3, None)
            )]
        );

        let (wakes, posted) = std::sync::mpsc::channel::<crate::Wake>();
        let rest = format!("edit ticket={} timeout=60000", ok.ticket);
        let lane = std::thread::spawn(move || {
            crate::control_edit::serve_await_edit(
                crate::control::Scope::Owner,
                &rest,
                &mut MainPost::injected(|wake| wakes.send(wake).is_ok(), hops),
                &mut || false,
                slice,
            )
        });
        let deliver = |app: &mut App, wake| match take_hop(wake) {
            crate::Wake::EditAwait(call) => app.await_edit(*call),
            crate::Wake::EditAwaitWithdraw { waiter } => app.withdraw_edit_waiter(waiter),
            other => panic!("unexpected {other:?}"),
        };
        deliver(&mut app, posted.recv_timeout(HANG).expect("the wait"));
        type_text(&mut app, wid, "edited ");
        let document = document_of(&app, view(&ok));
        let held = app.hold_document_save_for_test(document);
        app.land_held_document_save_for_test(document, view(&ok), held)
            .unwrap();
        let durable = app.document_store.checkpoint_seq(document).unwrap();
        app.close_active_native_tab(wid).unwrap();
        deliver(&mut app, posted.recv_timeout(HANG).expect("the withdrawal"));
        assert_eq!(
            lane.join().unwrap(),
            format!("OK edit done result=closed saved=1 seq={}\n", durable.0)
        );
        assert_eq!(
            std::fs::read_to_string(&f.path).unwrap(),
            "one\ntwo\nedited three\nfour\n"
        );
        assert!(app.native_documents.edits.waiters.is_empty());
    }

    fn press(app: &mut App, wid: WindowId, chord: &str) {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
        for key in chord.split(' ') {
            let letter = key
                .strip_prefix("C-")
                .and_then(|rest| rest.chars().next())
                .expect("a C- key");
            let event = crate::input::InputEvent::Key {
                key: Key::Character(letter),
                mods: Modifiers::CTRL,
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            };
            assert!(app.native_input_event(wid, &event), "{chord}");
        }
    }

    fn status(app: &App, view: ViewId) -> Option<String> {
        match app.native_runtime.view_state(view) {
            Some(AppViewState::Editor(state)) => state.status.clone(),
            _ => panic!("an Editor view"),
        }
    }

    fn buffer_text(app: &App, view: ViewId) -> String {
        let document = document_of(app, view);
        app.document_store
            .snapshot(document)
            .unwrap()
            .text
            .as_ref()
            .to_string()
    }

    fn header_row(app: &App, wid: WindowId) -> Option<(String, bool, bool)> {
        use crate::native_ui::UiKey;
        let compiled = app.compiled_native_ui(wid).expect("the view compiles");
        let row = compiled.semantic(&UiKey::new("editor/edit-header"))?;
        let part = |key: &str| {
            compiled.semantic(&UiKey::new(key)).is_some_and(|node| {
                node.parent.as_ref() == Some(&row.key)
                    && node.action.as_ref().map(|action| action.as_str()) == Some(key)
            })
        };
        Some((
            row.label.clone(),
            part("editor/finish"),
            part("editor/abort"),
        ))
    }

    fn edit_action_gates(app: &App, wid: WindowId, action: &str) -> (bool, bool) {
        let (instance, view) = app.active_native_view(wid).unwrap();
        let commands = app.native_runtime.commands(instance, view).unwrap();
        let command = commands
            .iter()
            .find(|command| command.id.as_str() == action)
            .unwrap();
        let compiled = app.compiled_native_ui(wid).unwrap();
        let control = compiled
            .semantic(&crate::native_ui::UiKey::new(action))
            .unwrap();
        (command.enabled, control.state.unwrap().enabled)
    }

    fn edit_buffer(app: &App, view: ViewId) -> &crate::native_editor::EditorBufferView {
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("an Editor view")
        };
        state.buffer.as_ref().unwrap()
    }

    fn outcome(answer: &Receiver<String>) -> EditOutcome {
        EditOutcome::parse(&answer.try_recv().expect("answered")).unwrap()
    }

    #[test]
    fn finish_saves_then_answers_finished_saved_1_after_ack() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        let f = fixture("finish", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "fix: ");
        let document = document_of(&app, edit_view);
        let held = app.hold_document_save_for_test(document);
        press(&mut app, wid, "C-c C-c");
        assert_eq!(
            status(&app, edit_view).as_deref(),
            Some("Saving, then closing")
        );
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty), "not yet acked");
        assert!(app.view_store.get(edit_view).is_some());
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "base\n");

        app.land_held_document_save_for_test(document, edit_view, held)
            .unwrap();
        let durable = app.document_store.checkpoint_seq(document).unwrap();
        assert_eq!(
            outcome(&answer),
            EditOutcome {
                result: EditResult::Finished,
                saved: true,
                seq: durable.0,
            }
        );
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "fix: base\n");
        assert!(
            app.view_store.get(edit_view).is_none(),
            "done closes the view"
        );
        assert_eq!(
            active_tab(&app, wid),
            caller_tab,
            "handed back to the caller"
        );
        assert_ne!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));

        let g = fixture("finish-clean", b"as is\n");
        let (ticket, _) = edit(&mut app, sid, request(&g.uri, None));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        press(&mut app, wid, "C-c C-c");
        let done = outcome(&answer);
        assert_eq!((done.result, done.saved), (EditResult::Finished, false));
    }

    #[test]
    fn abort_discards_and_answers_aborted() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        let f = fixture("abort", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "never mind ");
        press(&mut app, wid, "C-c C-k");
        let aborted = outcome(&answer);
        assert_eq!(
            (aborted.result, aborted.saved),
            (EditResult::Aborted, false)
        );
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "base\n");
        assert!(app.view_store.get(edit_view).is_none());
        assert_eq!(active_tab(&app, wid), caller_tab);

        let g = fixture("abort-after-save", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "kept ");
        app.save_document_checkpoint(document_of(&app, edit_view), edit_view)
            .unwrap();
        type_text(&mut app, wid, "dropped ");
        press(&mut app, wid, "C-c C-k");
        let aborted = outcome(&answer);
        assert_eq!(
            (aborted.result, aborted.saved),
            (EditResult::Aborted, false)
        );
        assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "kept base\n");
    }

    #[test]
    fn finish_without_ticket_refuses_and_keeps_the_view() {
        let wid = WindowId(0);
        let mut app = app();
        let f = fixture("finish-none", b"mine\n");
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, mine) = app.active_native_view(wid).unwrap();
        type_text(&mut app, wid, "typed ");
        press(&mut app, wid, "C-c C-c");
        assert_eq!(status(&app, mine).as_deref(), Some(NO_EDIT_WAITING));
        assert!(app.view_store.get(mine).is_some(), "the view stays");
        assert_eq!(buffer_text(&app, mine), "typed mine\n");
        assert_eq!(
            std::fs::read_to_string(&f.path).unwrap(),
            "mine\n",
            "nothing was saved"
        );

        let canonical =
            crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&f.path).unwrap())
                .unwrap();
        let (_, answer) = wait(&mut app, AwaitKey::Uri(canonical));
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
        press(&mut app, wid, "C-c C-c");
        let done = outcome(&answer);
        assert_eq!((done.result, done.saved), (EditResult::Finished, true));
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "typed mine\n");
        assert!(app.view_store.get(mine).is_none());
    }

    #[test]
    fn abort_without_waiter_refuses_and_keeps_text() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);

        let f = fixture("abort-none", b"mine\n");
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, mine) = app.active_native_view(wid).unwrap();
        type_text(&mut app, wid, "typed ");
        press(&mut app, wid, "C-c C-k");
        assert_eq!(status(&app, mine).as_deref(), Some(NO_EDIT_WAITING));
        assert_eq!(buffer_text(&app, mine), "typed mine\n", "nothing discarded");
        assert!(app.view_store.get(mine).is_some());

        select(&mut app, wid, caller_tab);
        let g = fixture("abort-orphan", b"theirs\n");
        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let orphan = view(&ok(&line));
        let (waiter, _gone) = wait(&mut app, AwaitKey::Ticket(ticket));
        let left = Instant::now();
        app.withdraw_edit_waiter_at(waiter, left);
        app.sweep_orphaned_edits(left + ORPHAN_AFTER);
        assert!(orphaned(&app, ticket));
        type_text(&mut app, wid, "typed ");
        press(&mut app, wid, "C-c C-k");
        assert_eq!(status(&app, orphan).as_deref(), Some(ABORT_GONE));
        assert_eq!(buffer_text(&app, orphan), "typed theirs\n");
        assert!(app.view_store.get(orphan).is_some());
        press(&mut app, wid, "C-c C-c");
        assert!(app.view_store.get(orphan).is_none());
        assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "typed theirs\n");
        let (_, late) = wait(&mut app, AwaitKey::Ticket(ticket));
        let done = outcome(&late);
        assert_eq!((done.result, done.saved), (EditResult::Finished, true));

        select(&mut app, wid, caller_tab);
        let h = fixture("abort-unwaited", b"open\n");
        let mut unwaited = request(&h.uri, None);
        unwaited.wait = false;
        let (ticket, line) = edit(&mut app, sid, unwaited);
        let free = view(&ok(&line));
        app.sweep_orphaned_edits(Instant::now() + ORPHAN_AFTER * 4);
        assert!(
            !orphaned(&app, ticket),
            "an unwaited ticket is not orphaned"
        );
        type_text(&mut app, wid, "typed ");
        press(&mut app, wid, "C-c C-k");
        assert_eq!(status(&app, free).as_deref(), Some(ABORT_UNWAITED));
        assert_eq!(buffer_text(&app, free), "typed open\n");
        press(&mut app, wid, "C-c C-c");
        assert!(app.view_store.get(free).is_none());
        assert_eq!(std::fs::read_to_string(&h.path).unwrap(), "typed open\n");
        assert_eq!(
            app.native_documents
                .edits
                .outcome_of(ticket)
                .map(|o| o.result),
            Some(EditResult::Finished)
        );
    }

    #[test]
    fn header_row_only_on_edit_views() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        let f = fixture("header", b"subject\n");
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, mine) = app.active_native_view(wid).unwrap();
        assert_eq!(header_row(&app, wid), None, "the person's own view");

        select(&mut app, wid, caller_tab);
        let mut asked = request(&f.uri, None);
        asked.for_word = Some("git".to_string());
        let (ticket, line) = edit(&mut app, sid, asked);
        let edit_view = view(&ok(&line));
        let (waiter, _answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(
            header_row(&app, wid),
            Some(("Editing for git".to_string(), true, true))
        );
        let (mine_tab, edit_tab) = (tab_of(&app, wid, mine), tab_of(&app, wid, edit_view));
        select(&mut app, wid, mine_tab);
        assert_eq!(header_row(&app, wid), None);
        select(&mut app, wid, edit_tab);

        let left = Instant::now();
        app.withdraw_edit_waiter_at(waiter, left);
        app.sweep_orphaned_edits(left + ORPHAN_AFTER);
        assert_eq!(
            header_row(&app, wid),
            Some((
                "The program waiting on this edit has gone".to_string(),
                true,
                false
            ))
        );
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(
            header_row(&app, wid),
            Some(("Editing for git".to_string(), true, true))
        );

        type_text(&mut app, wid, "fix ");
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = document_of(&app, edit_view);
        let draft = app.document_store.snapshot(document).unwrap();
        let mut capability_failures = Vec::new();
        for prompt in [
            crate::native_editor::EditorCommand::GotoLine,
            crate::native_editor::EditorCommand::ExecuteCommand,
            crate::native_editor::EditorCommand::SwitchBuffer,
            crate::native_editor::EditorCommand::KillBuffer,
            crate::native_editor::EditorCommand::IncrementalSearch,
        ] {
            app.dispatch_native_event(wid, AppEvent::EditorCommand(prompt))
                .unwrap();
            type_text(&mut app, wid, "2");
            assert!(edit_buffer(&app, edit_view).minibuffer_active());
            let before = edit_buffer(&app, edit_view).clone();
            for action in ["editor/finish", "editor/abort"] {
                let gates = edit_action_gates(&app, wid, action);
                if gates != (false, false) {
                    capability_failures.push(format!(
                        "{prompt:?}: {action} command/toolbar enabled: {gates:?}"
                    ));
                }
                if app
                    .act_app(crate::app_control::ActRequest {
                        view: edit_view,
                        ui_key: action.to_string(),
                        action: action.to_string(),
                        value: None,
                    })
                    .is_ok()
                {
                    capability_failures.push(format!("{prompt:?}: {action} act did not refuse"));
                }
                assert_eq!(edit_buffer(&app, edit_view), &before);
                if app
                    .activate_native_palette_target(&crate::palette::NativeCommandTarget {
                        window: wid,
                        instance,
                        view: edit_view,
                        generation: app.native_runtime.view_generation(edit_view).unwrap(),
                        action: crate::native_ui::ActionId::new(action),
                    })
                    .is_ok()
                {
                    capability_failures
                        .push(format!("{prompt:?}: {action} palette did not refuse"));
                }
                assert_eq!(edit_buffer(&app, edit_view), &before);
                assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
                assert_eq!(
                    app.native_documents.edits.ticket_of_view(edit_view),
                    Some(ticket)
                );
                assert!(!app.native_documents.edits.endings.contains_key(&edit_view));
                let after = app.document_store.snapshot(document).unwrap();
                assert_eq!((after.seq, after.text), (draft.seq, draft.text.clone()));
                assert_eq!(app.document_store.dirty(document), Some(true));
                assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "subject\n");
            }
            app.dispatch_native_event(
                wid,
                AppEvent::EditorCommand(crate::native_editor::EditorCommand::Abort),
            )
            .unwrap();
            assert!(!edit_buffer(&app, edit_view).minibuffer_active());
            for action in ["editor/finish", "editor/abort"] {
                assert_eq!(edit_action_gates(&app, wid, action), (true, true));
            }
        }
        app.act_app(crate::app_control::ActRequest {
            view: edit_view,
            ui_key: "editor/finish".to_string(),
            action: "editor/finish".to_string(),
            value: None,
        })
        .unwrap();
        let done = outcome(&answer);
        assert_eq!((done.result, done.saved), (EditResult::Finished, true));
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "fix subject\n");
        assert!(app.view_store.get(edit_view).is_none());
        assert!(
            app.view_store.get(mine).is_some(),
            "the person's view stays"
        );

        assert!(capability_failures.is_empty(), "{capability_failures:#?}");

        select(&mut app, wid, caller_tab);
        let g = fixture("header-click", b"body\n");
        let mut unwaited = request(&g.uri, None);
        unwaited.wait = false;
        let (ticket, line) = edit(&mut app, sid, unwaited);
        let clicked = view(&ok(&line));
        assert_eq!(
            header_row(&app, wid),
            Some(("Nothing waits on this edit".to_string(), true, false))
        );
        type_text(&mut app, wid, "clicked ");
        use crate::app_mouse::{NativePointerTarget, PointerAction};
        app.pointer_cmd(PointerAction::MoveNative(NativePointerTarget::Key(
            "editor/finish".to_string(),
        )))
        .expect("the pointer reaches done");
        app.pointer_cmd(PointerAction::Click).expect("the click");
        assert!(app.view_store.get(clicked).is_none());
        assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "clicked body\n");
        assert_eq!(
            app.native_documents
                .edits
                .outcome_of(ticket)
                .map(|o| o.result),
            Some(EditResult::Finished)
        );
    }

    #[test]
    fn create_1_opens_a_missing_file_and_first_save_creates_it() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let f = fixture("create", b"");
        let path = f.path.with_file_name("new.txt");
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        let mut asked = request(&uri, None);
        asked.create = true;
        let (ticket, line) = edit(&mut app, sid, asked);
        let edit_view = view(&ok(&line));
        assert!(!path.exists(), "opening creates nothing");
        assert_eq!(buffer_text(&app, edit_view), "");
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "first line\n");
        press(&mut app, wid, "C-c C-c");
        let done = outcome(&answer);
        assert_eq!((done.result, done.saved), (EditResult::Finished, true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first line\n");

        let other = f.path.with_file_name("never.txt");
        let uri = crate::native_document_host::path_to_file_uri(&other).unwrap();
        let mut asked = request(&uri, None);
        asked.create = true;
        let (ticket, _) = edit(&mut app, sid, asked);
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "unwanted");
        press(&mut app, wid, "C-c C-k");
        let aborted = outcome(&answer);
        assert_eq!(
            (aborted.result, aborted.saved),
            (EditResult::Aborted, false)
        );
        assert!(!other.exists(), "abort creates nothing");
    }

    #[test]
    fn missing_without_create_is_refused() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let tabs = tab_ids(&app, wid);
        let f = fixture("missing", b"");
        let path = f.path.with_file_name("absent.txt");
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        assert_eq!(
            edit(&mut app, sid, request(&uri, None)).1,
            "ERR refused missing\n"
        );
        assert_eq!(tab_ids(&app, wid), tabs, "nothing opened");
        assert!(!path.exists());
        let mut asked = request(
            &crate::native_document_host::path_to_file_uri(f.path.parent().unwrap()).unwrap(),
            None,
        );
        asked.create = true;
        assert_eq!(edit(&mut app, sid, asked).1, "ERR refused not-regular\n");
        assert!(app.native_documents.edits.open.is_empty());
    }

    #[test]
    fn crlf_held_buffer_takes_typed_line_ends_as_its_own() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let f = fixture("crlf", b"one\r\ntwo\r\n");
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(buffer_text(&app, edit_view), "one\ntwo\n");
        type_text(&mut app, wid, "zero\r\n");
        assert_eq!(buffer_text(&app, edit_view), "zero\none\ntwo\n");
        press(&mut app, wid, "C-c C-c");
        let done = outcome(&answer);
        assert_eq!((done.result, done.saved), (EditResult::Finished, true));
        assert_eq!(std::fs::read(&f.path).unwrap(), b"zero\r\none\r\ntwo\r\n");
    }

    #[test]
    fn abort_keeps_text_the_edit_did_not_type() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        let f = fixture("abort-mine", b"base\n");
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, mine) = app.active_native_view(wid).unwrap();
        type_text(&mut app, wid, "mine ");
        select(&mut app, wid, caller_tab);
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "theirs ");
        press(&mut app, wid, "C-c C-k");
        let aborted = outcome(&answer);
        assert_eq!(
            (aborted.result, aborted.saved),
            (EditResult::Aborted, false)
        );
        assert!(app.view_store.get(edit_view).is_none());
        let kept = buffer_text(&app, mine);
        assert!(
            kept.contains("mine ") && kept.contains("theirs "),
            "{kept:?}"
        );
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "base\n");
        assert_eq!(status(&app, mine).as_deref(), Some(ABORT_KEPT));

        let g = fixture("abort-mine-alone", b"base\n");
        app.open_document_tab(AppKind::Editor, &g.uri).unwrap();
        let (_, mine) = app.active_native_view(wid).unwrap();
        type_text(&mut app, wid, "mine ");
        select(&mut app, wid, caller_tab);
        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        let edit_tab = tab_of(&app, wid, edit_view);
        let mine_tab = tab_of(&app, wid, mine);
        select(&mut app, wid, mine_tab);
        app.close_active_native_tab(wid).unwrap();
        select(&mut app, wid, edit_tab);
        press(&mut app, wid, "C-c C-k");
        assert_eq!(status(&app, edit_view).as_deref(), Some(ABORT_FOREIGN));
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(buffer_text(&app, edit_view), "mine base\n");
        assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "base\n");
    }

    #[test]
    fn done_whose_save_does_not_land_says_why_and_stays() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let f = fixture("finish-halts", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "lost ");
        let document = document_of(&app, edit_view);
        let held = app.hold_document_save_for_test(document);
        press(&mut app, wid, "C-c C-c");
        std::fs::write(&f.path, "changed\n").unwrap();
        assert!(
            app.land_held_document_save_for_test(document, edit_view, held)
                .is_err()
        );
        assert_eq!(
            status(&app, edit_view),
            Some(format!(
                "Not done: {}",
                crate::native_app::DISK_CONFLICT_RECOVERY
            ))
        );
        assert!(app.view_store.get(edit_view).is_some(), "the view stays");
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
        assert!(!app.native_documents.edits.endings.contains_key(&edit_view));
        press(&mut app, wid, "C-c C-k");
        let aborted = outcome(&answer);
        assert_eq!(
            (aborted.result, aborted.saved),
            (EditResult::Aborted, false)
        );
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "changed\n");
    }

    #[test]
    fn abort_refused_while_done_saves_keeps_the_done() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let f = fixture("abort-during-done", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "kept ");
        let document = document_of(&app, edit_view);
        let held = app.hold_document_save_for_test(document);
        press(&mut app, wid, "C-c C-c");
        press(&mut app, wid, "C-c C-k");
        assert_eq!(status(&app, edit_view).as_deref(), Some(ABORT_SAVING));
        assert_eq!(buffer_text(&app, edit_view), "kept base\n");
        let (instance, _) = app.active_native_view(wid).unwrap();
        assert!(app.document_save_owed(document));
        let Some(crate::native_app::NativeApp::Editor(editor)) = app.native_runtime.app(instance)
        else {
            panic!("an Editor instance")
        };
        assert!(editor.checkpoint_pending);
        assert!(!app.can_save_document(wid));
        assert_eq!(edit_action_gates(&app, wid, "editor/finish"), (true, true));
        let generation = app.native_runtime.view_generation(edit_view).unwrap();
        let target = |action: &str| crate::palette::NativeCommandTarget {
            window: wid,
            instance,
            view: edit_view,
            generation,
            action: crate::native_ui::ActionId::new(action),
        };
        app.activate_native_palette_target(&target("editor/finish"))
            .unwrap();
        let ending = app.native_documents.edits.endings.get(&edit_view).copied();
        assert!(matches!(ending, Some(Ending::Finish { .. })));
        let before = edit_buffer(&app, edit_view).clone();
        let mut capability_failures = Vec::new();
        let gates = edit_action_gates(&app, wid, "editor/abort");
        if gates != (false, false) {
            capability_failures.push(format!("saving: Abort command/toolbar enabled: {gates:?}"));
        }
        if app
            .act_app(crate::app_control::ActRequest {
                view: edit_view,
                ui_key: "editor/abort".to_string(),
                action: "editor/abort".to_string(),
                value: None,
            })
            .is_ok()
        {
            capability_failures.push("saving: Abort act did not refuse".to_string());
        }
        assert_eq!(edit_buffer(&app, edit_view), &before);
        if app
            .activate_native_palette_target(&target("editor/abort"))
            .is_ok()
        {
            capability_failures.push("saving: Abort palette did not refuse".to_string());
        }
        assert_eq!(edit_buffer(&app, edit_view), &before);
        assert_eq!(
            app.native_documents.edits.endings.get(&edit_view),
            ending.as_ref()
        );
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "base\n");
        app.land_held_document_save_for_test(document, edit_view, held)
            .unwrap();
        let done = outcome(&answer);
        assert_eq!((done.result, done.saved), (EditResult::Finished, true));
        assert!(app.view_store.get(edit_view).is_none());
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "kept base\n");
        assert!(capability_failures.is_empty(), "{capability_failures:#?}");
    }

    #[test]
    fn done_behind_the_person_leaves_them_on_their_tab() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        let elsewhere = {
            caller(&mut app, wid);
            active_tab(&app, wid)
        };
        select(&mut app, wid, caller_tab);
        let g = fixture("finish-moved-on", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "pressed ");
        let document = document_of(&app, edit_view);
        let held = app.hold_document_save_for_test(document);
        press(&mut app, wid, "C-c C-c");
        select(&mut app, wid, elsewhere);
        app.land_held_document_save_for_test(document, edit_view, held)
            .unwrap();
        let done = outcome(&answer);
        assert_eq!((done.result, done.saved), (EditResult::Finished, true));
        assert!(app.view_store.get(edit_view).is_none());
        assert_eq!(active_tab(&app, wid), elsewhere, "the person stays put");

        select(&mut app, wid, caller_tab);
        let h = fixture("finish-beside", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&h.uri, None));
        let edit_view = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        let edit_tab = tab_of(&app, wid, edit_view);
        let (_, beside) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        focus_leaf(&mut app, wid, edit_view);
        type_text(&mut app, wid, "split ");
        let document = document_of(&app, edit_view);
        let held = app.hold_document_save_for_test(document);
        press(&mut app, wid, "C-c C-c");
        focus_leaf(&mut app, wid, beside);
        app.land_held_document_save_for_test(document, edit_view, held)
            .unwrap();
        let done = outcome(&answer);
        assert_eq!((done.result, done.saved), (EditResult::Finished, true));
        assert!(app.view_store.get(edit_view).is_none());
        assert_eq!(active_tab(&app, wid), edit_tab);
        assert_eq!(app.windows[&wid].tab_set.active().unwrap().focus, beside);
    }

    #[test]
    fn an_edit_tab_closed_from_behind_leaves_the_person_on_their_tab() {
        let wid = WindowId(0);
        for closer in ["close_tab_at", "close_editor_view"] {
            let mut app = app();
            let sid = caller(&mut app, wid);
            let caller_tab = active_tab(&app, wid);
            let elsewhere = {
                caller(&mut app, wid);
                active_tab(&app, wid)
            };
            select(&mut app, wid, caller_tab);
            let g = fixture(&format!("closed-behind-{closer}"), b"base\n");
            let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
            let edit_view = view(&ok(&line));
            let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
            let edit_tab = tab_of(&app, wid, edit_view);
            select(&mut app, wid, elsewhere);
            if closer == "close_tab_at" {
                let index = app.windows[&wid]
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|tab| tab.id == edit_tab)
                    .unwrap();
                assert!(!app.close_tab_at(wid, index));
            } else {
                app.close_editor_view(edit_view).unwrap();
            }
            assert!(app.view_store.get(edit_view).is_none(), "{closer}");
            assert_eq!(outcome(&answer).result, EditResult::Closed, "{closer}");
            assert_eq!(
                active_tab(&app, wid),
                elsewhere,
                "{closer}: the person stays put; the caller's tab stays behind"
            );
            assert_eq!(app.structural_invariant_violation(), None, "{closer}");
        }
    }

    #[test]
    fn an_edit_tab_moved_to_another_window_closes_without_turning_its_old_window() {
        let wid = WindowId(0);
        for closer in ["close_tab_at", "close_editor_view"] {
            let mut app = app();
            let sid = caller(&mut app, wid);
            let caller_tab = active_tab(&app, wid);
            let elsewhere = {
                caller(&mut app, wid);
                active_tab(&app, wid)
            };
            select(&mut app, wid, caller_tab);
            let g = fixture(&format!("moved-away-{closer}"), b"base\n");
            let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
            let edit_view = view(&ok(&line));
            let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
            assert_eq!(
                app.windows[&wid].tab_set.active().map(|tab| tab.focus),
                Some(edit_view),
                "PRECONDITION: the edit tab in front"
            );
            let moved = app
                .detach_active_tab_logical()
                .expect("the edit tab moves to a window of its own");
            select(&mut app, wid, elsewhere);

            if closer == "close_tab_at" {
                let _ = app.close_tab_at(moved, 0);
            } else {
                app.close_editor_view(edit_view).unwrap();
            }
            assert!(app.view_store.get(edit_view).is_none(), "{closer}");
            assert_eq!(outcome(&answer).result, EditResult::Closed, "{closer}");
            assert_eq!(
                active_tab(&app, wid),
                elsewhere,
                "{closer}: the old window stays on the tab the person left it on"
            );
            assert_eq!(app.structural_invariant_violation(), None, "{closer}");
        }
    }

    #[test]
    fn undo_back_to_a_mapped_files_text_reads_clean() {
        use crate::native_editor::EditorCommand;
        let wid = WindowId(0);
        for (name, bytes) in [
            ("undo-crlf", &b"one\r\ntwo\r\n"[..]),
            ("undo-bom", &b"\xef\xbb\xbfone\ntwo\n"[..]),
        ] {
            let f = fixture(name, bytes);
            let mut app = app();
            app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
            let (_, mine) = app.active_native_view(wid).unwrap();
            let document = document_of(&app, mine);
            type_text(&mut app, wid, "x");
            assert_eq!(app.document_store.dirty(document), Some(true), "{name}");
            app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::Undo))
                .unwrap();
            assert_eq!(buffer_text(&app, mine), "one\ntwo\n", "{name}");
            assert_eq!(
                app.document_store.dirty(document),
                Some(false),
                "{name}: the buffer is the file again"
            );
            assert_eq!(std::fs::read(&f.path).unwrap(), bytes, "{name}");
        }
    }

    /// A dirty CRLF or BOM document's draft rides an update: the journal's
    /// base is the text the file was admitted as, which is what the carry
    /// compares, not the file's bytes.
    #[cfg(unix)]
    #[test]
    fn a_mapped_documents_draft_is_carried() {
        let wid = WindowId(0);
        for (name, bytes) in [
            ("carry-crlf", &b"one\r\ntwo\r\n"[..]),
            ("carry-bom", &b"\xef\xbb\xbfone\ntwo\n"[..]),
        ] {
            let f = fixture(name, bytes);
            let mut app = App::headless_for_test();
            app.use_document_journal_root_for_test(f.path.with_file_name("drafts"));
            app.carry_drafts_as_windowed_for_test();
            app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
            let (_, mine) = app.active_native_view(wid).unwrap();
            let document = document_of(&app, mine);
            type_text(&mut app, wid, "x");
            assert_eq!(
                app.draft_carry(document),
                crate::app_documents::DraftCarry::Carried {
                    bytes: "xone\ntwo\n".len()
                },
                "{name}"
            );
        }
    }
}
